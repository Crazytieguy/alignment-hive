//! Server-level descriptions used at runtime.
//! Tool descriptions live as doc comments on each tool method in server.rs
//! (rmcp's #[tool] macro reads them automatically).

pub const SERVER_INSTRUCTIONS: &str = "\
MCP server for spinning up cloud GPU machines and interacting with persistent Jupyter kernels. \
Use start() to create a machine, execute() to run Python code, and stop()/terminate() to clean up.\n\
Multiple machines can run concurrently: give each a name via start(name=...) and pass `instance` \
to machine-scoped tools (with a single machine, `instance` can be omitted; kernel-scoped tools \
never need it — kernels are routed automatically). \
start() automatically reconnects to a machine from a previous session if one exists \
(resumes stopped machines where the runtime supports it). You'll need to create new kernels after reconnecting.\n\
On vast.ai, search_vast_offers() lists marketplace hosts with picking advice — rank a shortlist \
and pass it as start(vast_offers=[...]), or omit it to auto-pick the cheapest qualifying host.\n\
All executions are auto-saved as .ipynb notebook files (path shown in create_kernel output). \
Read these notebooks to recover context after conversation compaction.";
