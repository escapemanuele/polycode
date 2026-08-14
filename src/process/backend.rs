use std::path::Path;

use super::{
    BackendSessionId, ExitEvidence, ManagedProcess, ManagedProcessId, OutputChunk, OutputStream,
    ProcessError,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendAvailability {
    pub kind: &'static str,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendSessionState {
    Absent,
    Owned,
}

/// Synchronous process-supervisor boundary used below future provider adapters.
pub trait ProcessBackend {
    fn kind(&self) -> &'static str;

    fn session_id(&self, process_id: ManagedProcessId) -> BackendSessionId;

    /// Checks backend executable availability without creating a session.
    ///
    /// # Errors
    /// Returns typed missing-backend or command failures.
    fn availability(&self) -> Result<BackendAvailability, ProcessError>;

    /// Starts hidden runner from immutable manifest using backend-native ownership.
    ///
    /// # Errors
    /// Rejects foreign collisions and backend command failures.
    fn start(&self, process: &ManagedProcess, manifest: &Path) -> Result<(), ProcessError>;

    /// Inspects only concrete supervisor evidence, validating ownership.
    ///
    /// # Errors
    /// Rejects foreign sessions and backend command failures.
    fn inspect_session(
        &self,
        process: &ManagedProcess,
    ) -> Result<BackendSessionState, ProcessError>;

    /// Reads raw bytes at explicit offset without advancing durable cursor.
    ///
    /// # Errors
    /// Rejects truncated files and I/O failures.
    fn read_output(
        &self,
        process: &ManagedProcess,
        stream: OutputStream,
        offset: u64,
        max_bytes: usize,
    ) -> Result<OutputChunk, ProcessError>;

    /// Returns current byte length for one retained output stream.
    ///
    /// # Errors
    /// Returns I/O failures.
    fn output_length(
        &self,
        process: &ManagedProcess,
        stream: OutputStream,
    ) -> Result<u64, ProcessError>;

    /// Reads and validates atomic runner exit evidence.
    ///
    /// # Errors
    /// Rejects corrupt or foreign evidence.
    fn read_exit_evidence(
        &self,
        process: &ManagedProcess,
    ) -> Result<Option<ExitEvidence>, ProcessError>;

    /// Requests graceful interruption after validating session ownership.
    ///
    /// # Errors
    /// Rejects foreign sessions and backend command failures.
    fn interrupt(&self, process: &ManagedProcess) -> Result<(), ProcessError>;

    /// Removes only proven-owned supervisor resources. Output files stay retained.
    ///
    /// # Errors
    /// Rejects foreign sessions and backend command failures.
    fn cleanup(&self, process: &ManagedProcess) -> Result<(), ProcessError>;
}
