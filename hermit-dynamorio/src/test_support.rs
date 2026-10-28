//! Strict artifact holdings for the original native launch/placement controls.
//! No availability branch, artifact replacement, or legacy runtime fallback.

use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The exact production schema2 engine/client/SDK holding, retained by a test.
pub struct LaunchArtifacts {
    inner: crate::engine_artifact::NativeArtifacts,
    sdk_library: File,
}

impl LaunchArtifacts {
    /// Require the real source-matched current cohort through production code.
    pub fn required() -> anyhow::Result<Self> {
        let inner = crate::engine_artifact::NativeArtifacts::from_required_producer_input()?;
        let library = inner
            .drrun()
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow::anyhow!("SDK root beside actual drrun"))?
            .join("lib64/release/libdynamorio.so");
        let sdk_library = File::open(library)?;
        Ok(Self { inner, sdk_library })
    }
    /// Require the explicit observer feature in the actual compiled engine cohort.
    pub fn require_native_first_instruction_observer(&self) -> anyhow::Result<()> {
        self.inner.require_native_first_instruction_observer()
    }
    /// The actual SDK runner, never a wrapper or an install-directory override.
    pub fn runner(&self) -> &Path {
        self.inner.drrun()
    }
    /// The exact real shared-engine client selected by production.
    pub fn client(&self) -> &Path {
        self.inner.client()
    }
    /// Preserve production held/hash checks through success or failure teardown.
    pub fn recheck(&self) -> anyhow::Result<()> {
        self.inner.recheck()
    }
    /// Join actual pre-exec runner/client arguments to their held native cohort.
    pub fn check_observed(&self, runner: &[u8], client: &[u8]) -> anyhow::Result<()> {
        let runner = Path::new(std::ffi::OsStr::from_bytes(runner));
        let client = Path::new(std::ffi::OsStr::from_bytes(client));
        anyhow::ensure!(
            std::fs::canonicalize(runner)? == self.runner(),
            "actual runner changed"
        );
        anyhow::ensure!(
            std::fs::canonicalize(client)? == self.client(),
            "actual selected client changed"
        );
        let sdk = runner
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow::anyhow!("actual observed SDK root"))?;
        let observed = std::fs::metadata(sdk.join("lib64/release/libdynamorio.so"))?;
        let held = self.sdk_library.metadata()?;
        anyhow::ensure!(
            (held.dev(), held.ino()) == (observed.dev(), observed.ino()),
            "actual launch used another SDK library inode"
        );
        self.recheck()
    }
}
