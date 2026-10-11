//! A rehearsal of the executor against a private scratch copy of the journal.
//!
//! Opening a journal for writing repairs a torn tail, and the executor makes some of its
//! refusals (an inconsistent journal, a changed derived request, a rerun with nothing to rerun)
//! only after that. The read-only checks in this module's parent mirror the ones it can name;
//! the rehearsal is the exact backstop. It runs the same executor, plan, profiles, authority,
//! limits and reruns over a copy of the journal with an already cancelled token and synthetic
//! adapters that declare the same settings support as the hosted ones. Cancellation is checked
//! before any request is reserved, so nothing is dispatched, and the copy is deleted before the
//! rehearsal returns. Only when it succeeds is the real journal opened.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ohand_core::providers::contracts::{CancelToken, DispatchLimits, SystemClock};

use super::authority::ScratchAuthority;
use super::profiles::SyntheticAdapter;
use super::{Prepared, Refusal};
use crate::executor::{ArmBinding, Executor, ExecutorLimits, RunControls, RunRequest, SideKey};

const SCRATCH_JOURNAL: &str = "rehearsal.ndjson";

/// A private directory removed, with everything in it, when dropped.
struct ScratchDirectory {
    path: PathBuf,
}

impl ScratchDirectory {
    fn create() -> Result<ScratchDirectory, Refusal> {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "ohand-bench-rehearsal-{}-{nanos}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&path).map_err(|_| {
            Refusal("a private scratch copy of the journal could not be made".to_string())
        })?;
        Ok(ScratchDirectory { path })
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Refuses whatever the executor would refuse for this plan and `journal`, without writing
/// `journal` and without dispatching anything. `journal` need not exist yet.
pub(super) fn rehearse(
    journal: &Path,
    prepared: &Prepared,
    authority: &ScratchAuthority,
    limits: ExecutorLimits,
    reruns: &[SideKey],
) -> Result<(), Refusal> {
    let scratch = ScratchDirectory::create()?;
    let copy = scratch.path.join(SCRATCH_JOURNAL);
    if journal.exists() {
        std::fs::copy(journal, &copy).map_err(|_| {
            Refusal("a private scratch copy of the journal could not be made".to_string())
        })?;
    }
    let refused = |error: crate::executor::ExecutorError| Refusal(error.to_string());
    let mut executor = Executor::open(&copy, prepared.experiment.clone()).map_err(refused)?;
    let adapter_a = SyntheticAdapter::new(prepared.arm_a.spec.protocol, "a");
    let adapter_b = SyntheticAdapter::new(prepared.arm_b.spec.protocol, "b");
    let cancelled = CancelToken::new();
    cancelled.cancel();
    let clock = SystemClock::new();
    executor
        .run(RunRequest {
            cases: &prepared.corpus.cases,
            arm_a: ArmBinding {
                profile: &prepared.arm_a.profile,
                adapter: &adapter_a,
                authorizer: authority,
            },
            arm_b: ArmBinding {
                profile: &prepared.arm_b.profile,
                adapter: &adapter_b,
                authorizer: authority,
            },
            controls: RunControls {
                clock: &clock,
                cancel: &cancelled,
                dispatch_limits: DispatchLimits::default(),
            },
            limits,
            reruns,
        })
        .map_err(refused)?;
    Ok(())
}
