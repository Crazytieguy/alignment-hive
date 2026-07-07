//! Server-level descriptions used at runtime.
//! Tool descriptions live as doc comments on each tool method in server.rs
//! (rmcp's #[tool] macro reads them automatically).

use std::path::Path;

/// Server instructions injected into every session with the server enabled.
/// Cross-cutting state Claude can't learn from any single tool description;
/// what each tool does belongs in that tool's own description.
pub fn server_instructions(project_dir: &Path) -> String {
    format!(
        "Cloud GPU machines with persistent Jupyter kernels.\n\
         Machines are named; when several are active, machine-scoped tools take an `instance` \
         argument (kernel-scoped tools never do — each kernel id routes to its machine \
         automatically). Starting a machine reconnects to the same-named machine from a \
         previous session if one exists; kernels don't survive that, so create new ones.\n\
         Everything executed on a kernel is auto-saved to a local .ipynb notebook (path shown \
         when the kernel is created) — read it to recover context after conversation compaction.\n\
         sync() and download() are rooted at the project directory the server started in: \
         {} (fixed for the server's lifetime — changing directories or entering a worktree \
         does not move it).",
        project_dir.display()
    )
}
