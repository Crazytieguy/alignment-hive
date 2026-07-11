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
         Typical flow: start() a machine → create_kernel() → sync() project files → execute() \
         code (kernel state persists across calls) → download() results → stop() or terminate() \
         when done (configured automatic cleanup is the backstop).\n\
         start() always creates a fresh machine; its optional label is display-only. status() \
         lists durable machines; attach(machine_id) reconnects to one — needed after this server \
         process restarts. When several machines are active, machine-scoped tools take an \
         `instance` argument; kernel-scoped tools never do (each kernel id routes to its machine \
         automatically). Machines are single-controller: one session drives a machine at a \
         time, and sharing one across sessions is unsupported — a fenced error means \
         another session took over; stop using that machine here (attach(force=true) is \
         the deliberate way to move control).\n\
         Everything executed is auto-saved to a local .ipynb notebook (path shown at kernel \
         creation) — read it to recover context after conversation compaction.\n\
         For long cells, prefer wait() over polling — holding the call open keeps a background \
         session alive; with no kernel_id, wait() covers every pending execution. Polling with \
         get_output() is fine when there is other work to do meanwhile.\n\
         sync() and download() are rooted at the project directory: {} (fixed for the server's \
         lifetime — changing directories or entering a worktree does not move it).",
        project_dir.display()
    )
}
