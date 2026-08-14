use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::model::RuntimeEvidence;
use super::{
    BackendAvailability, BackendSessionId, BackendSessionState, ExitEvidence, ManagedProcess,
    ManagedProcessId, OutputChunk, OutputStream, ProcessBackend, ProcessError,
};

pub(crate) const OWNER_PROCESS_ENV: &str = "POLYCODE_MANAGED_PROCESS_ID";
pub(crate) const OWNER_FINGERPRINT_ENV: &str = "POLYCODE_COMMAND_FINGERPRINT";
const MAX_OUTPUT_READ: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TmuxBackend {
    executable: OsString,
    runner_executable: PathBuf,
    socket_name: Option<OsString>,
}

impl TmuxBackend {
    #[must_use]
    pub fn new(runner_executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: OsString::from("tmux"),
            runner_executable: runner_executable.into(),
            socket_name: None,
        }
    }

    #[must_use]
    pub fn with_executables(
        executable: impl Into<OsString>,
        runner_executable: impl Into<PathBuf>,
    ) -> Self {
        Self {
            executable: executable.into(),
            runner_executable: runner_executable.into(),
            socket_name: None,
        }
    }

    /// Uses isolated tmux server socket, primarily for integration tests.
    #[must_use]
    pub fn with_socket_name(mut self, socket_name: impl Into<OsString>) -> Self {
        self.socket_name = Some(socket_name.into());
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        if let Some(socket_name) = &self.socket_name {
            command.arg("-L").arg(socket_name);
        }
        command
    }

    fn output(&self, operation: &'static str, args: &[&OsStr]) -> Result<Output, ProcessError> {
        let mut command = self.command();
        command.args(args);
        Self::execute(operation, &mut command)
    }

    fn execute(operation: &'static str, command: &mut Command) -> Result<Output, ProcessError> {
        command.output().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProcessError::TmuxNotFound
            } else {
                ProcessError::TmuxCommand {
                    operation,
                    message: error.kind().to_string(),
                }
            }
        })
    }

    fn require_success(
        &self,
        operation: &'static str,
        args: &[&OsStr],
    ) -> Result<Output, ProcessError> {
        let output = self.output(operation, args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(ProcessError::TmuxCommand {
                operation,
                message: sanitized_stderr(&output.stderr),
            })
        }
    }

    fn exact_target(session: &BackendSessionId) -> OsString {
        OsString::from(format!("={}", session.as_str()))
    }

    fn pane_target(session: &BackendSessionId) -> OsString {
        OsString::from(format!("={}:0.0", session.as_str()))
    }

    fn session_environment(
        &self,
        session: &BackendSessionId,
        name: &str,
    ) -> Result<Option<String>, ProcessError> {
        let target = Self::exact_target(session);
        let output = self.output(
            "inspect session environment",
            &[
                OsStr::new("show-environment"),
                OsStr::new("-t"),
                &target,
                OsStr::new(name),
            ],
        )?;
        if !output.status.success() {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&output.stdout);
        Ok(line
            .trim_end()
            .strip_prefix(name)
            .and_then(|value| value.strip_prefix('='))
            .map(ToOwned::to_owned))
    }

    fn session_exists(&self, session: &BackendSessionId) -> Result<bool, ProcessError> {
        let target = Self::exact_target(session);
        Ok(self
            .output(
                "inspect session",
                &[OsStr::new("has-session"), OsStr::new("-t"), &target],
            )?
            .status
            .success())
    }

    fn output_path(process: &ManagedProcess, stream: OutputStream) -> &Path {
        match stream {
            OutputStream::Stdout => process.spec().stdout_path(),
            OutputStream::Stderr => process.spec().stderr_path(),
        }
    }

    fn exit_path(process: &ManagedProcess) -> Result<PathBuf, ProcessError> {
        process
            .spec()
            .stdout_path()
            .parent()
            .map(|directory| directory.join("exit.json"))
            .ok_or(ProcessError::InvalidSpec("stdout path has no parent"))
    }

    fn runtime_path(process: &ManagedProcess) -> Result<PathBuf, ProcessError> {
        process
            .spec()
            .stdout_path()
            .parent()
            .map(|directory| directory.join("runtime.json"))
            .ok_or(ProcessError::InvalidSpec("stdout path has no parent"))
    }

    fn pane_pid(&self, process: &ManagedProcess) -> Result<u32, ProcessError> {
        let target = Self::pane_target(process.backend_session_id());
        let output = self.require_success(
            "inspect runner PID",
            &[
                OsStr::new("display-message"),
                OsStr::new("-p"),
                OsStr::new("-t"),
                &target,
                OsStr::new("#{pane_pid}"),
            ],
        )?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| ProcessError::MissingRuntimeEvidence(process.id()))
    }

    fn runtime_evidence(&self, process: &ManagedProcess) -> Result<RuntimeEvidence, ProcessError> {
        let path = Self::runtime_path(process)?;
        let bytes =
            std::fs::read(path).map_err(|_| ProcessError::MissingRuntimeEvidence(process.id()))?;
        let evidence: RuntimeEvidence = serde_json::from_slice(&bytes)
            .map_err(|_| ProcessError::MissingRuntimeEvidence(process.id()))?;
        evidence.validate_for(process)?;
        if evidence.runner_pid() != self.pane_pid(process)? {
            return Err(ProcessError::MissingRuntimeEvidence(process.id()));
        }
        Ok(evidence)
    }
}

impl ProcessBackend for TmuxBackend {
    fn kind(&self) -> &'static str {
        "tmux"
    }

    fn session_id(&self, process_id: ManagedProcessId) -> BackendSessionId {
        BackendSessionId::for_process(process_id)
    }

    fn availability(&self) -> Result<BackendAvailability, ProcessError> {
        let output = self.require_success("version check", &[OsStr::new("-V")])?;
        Ok(BackendAvailability {
            kind: self.kind(),
            version: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        })
    }

    fn start(&self, process: &ManagedProcess, manifest: &Path) -> Result<(), ProcessError> {
        match self.inspect_session(process)? {
            BackendSessionState::Owned => return Ok(()),
            BackendSessionState::Absent => {}
        }
        let mut command = self.command();
        command
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(process.backend_session_id().as_str())
            .arg("-c")
            .arg(process.spec().working_directory());
        for (key, value) in std::env::vars_os() {
            command.arg("-e").arg(environment_assignment(key, &value));
        }
        command
            .arg("-e")
            .arg(format!("{OWNER_PROCESS_ENV}={}", process.id()))
            .arg("-e")
            .arg(format!(
                "{OWNER_FINGERPRINT_ENV}={}",
                process.command_fingerprint()
            ))
            .arg("--")
            .arg(&self.runner_executable)
            .arg("__run-process")
            .arg(manifest);
        let output = Self::execute("start session", &mut command)?;
        if output.status.success() {
            return Ok(());
        }
        if self.inspect_session(process)? == BackendSessionState::Owned {
            return Ok(());
        }
        Err(ProcessError::TmuxCommand {
            operation: "start session",
            message: sanitized_stderr(&output.stderr),
        })
    }

    fn inspect_session(
        &self,
        process: &ManagedProcess,
    ) -> Result<BackendSessionState, ProcessError> {
        if !self.session_exists(process.backend_session_id())? {
            return Ok(BackendSessionState::Absent);
        }
        let process_owner =
            self.session_environment(process.backend_session_id(), OWNER_PROCESS_ENV)?;
        let fingerprint_owner =
            self.session_environment(process.backend_session_id(), OWNER_FINGERPRINT_ENV)?;
        if process_owner.as_deref() == Some(process.id().to_string().as_str())
            && fingerprint_owner.as_deref() == Some(process.command_fingerprint())
        {
            Ok(BackendSessionState::Owned)
        } else if !self.session_exists(process.backend_session_id())? {
            Ok(BackendSessionState::Absent)
        } else {
            Err(ProcessError::ForeignSession {
                session_id: process.backend_session_id().clone(),
            })
        }
    }

    fn read_output(
        &self,
        process: &ManagedProcess,
        stream: OutputStream,
        offset: u64,
        max_bytes: usize,
    ) -> Result<OutputChunk, ProcessError> {
        if max_bytes == 0 || max_bytes > MAX_OUTPUT_READ {
            return Err(ProcessError::InvalidReadSize(MAX_OUTPUT_READ));
        }
        let path = Self::output_path(process, stream);
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        if length < offset {
            return Err(ProcessError::OutputTruncated(process.id()));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
        file.take(
            u64::try_from(max_bytes).map_err(|_| ProcessError::InvalidReadSize(MAX_OUTPUT_READ))?,
        )
        .read_to_end(&mut bytes)?;
        OutputChunk::new(
            process.id(),
            stream,
            process.cursor(stream).revision(),
            offset,
            bytes,
        )
    }

    fn output_length(
        &self,
        process: &ManagedProcess,
        stream: OutputStream,
    ) -> Result<u64, ProcessError> {
        Ok(std::fs::metadata(Self::output_path(process, stream))?.len())
    }

    fn read_exit_evidence(
        &self,
        process: &ManagedProcess,
    ) -> Result<Option<ExitEvidence>, ProcessError> {
        let path = Self::exit_path(process)?;
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let evidence: ExitEvidence =
            serde_json::from_slice(&bytes).map_err(|_| ProcessError::InvalidExitEvidence {
                process_id: process.id(),
                reason: "invalid JSON",
            })?;
        evidence.validate_for(process)?;
        Ok(Some(evidence))
    }

    fn interrupt(&self, process: &ManagedProcess) -> Result<(), ProcessError> {
        if self.inspect_session(process)? == BackendSessionState::Absent {
            return Ok(());
        }
        let runtime = self.runtime_evidence(process)?;
        let process_group = format!("-{}", runtime.process_group_id());
        let output = Command::new("/bin/kill")
            .arg("-INT")
            .arg(process_group)
            .output()
            .map_err(|error| ProcessError::SignalCommand(error.kind().to_string()))?;
        if !output.status.success() {
            return Err(ProcessError::SignalCommand(sanitized_stderr(
                &output.stderr,
            )));
        }
        Ok(())
    }

    fn cleanup(&self, process: &ManagedProcess) -> Result<(), ProcessError> {
        if self.inspect_session(process)? == BackendSessionState::Absent {
            return Ok(());
        }
        let target = Self::exact_target(process.backend_session_id());
        self.require_success(
            "cleanup session",
            &[OsStr::new("kill-session"), OsStr::new("-t"), &target],
        )?;
        Ok(())
    }
}

fn sanitized_stderr(stderr: &[u8]) -> String {
    const LIMIT: usize = 512;
    let prefix = &stderr[..stderr.len().min(LIMIT)];
    let message = String::from_utf8_lossy(prefix).trim().to_owned();
    if message.is_empty() {
        "unknown tmux failure".to_owned()
    } else {
        message
    }
}

fn environment_assignment(mut key: OsString, value: &OsStr) -> OsString {
    key.push("=");
    key.push(value);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tmux_binary_is_typed() {
        let backend = TmuxBackend::with_executables(
            "/definitely/missing/polycode-tmux",
            "/definitely/missing/polycode-runner",
        );
        assert!(matches!(
            backend.availability(),
            Err(ProcessError::TmuxNotFound)
        ));
    }
}
