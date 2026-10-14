//! Glyphst's side of external agents (Claude Code, Codex): the MCP server
//! that gives them the Reader's tools, the drivers that run their turns,
//! and the working folders whose checkpoints make their files undoable.

pub mod claude;
pub mod codex;
pub mod executable;
pub mod external;
pub mod mcp;
pub mod mcp_stdio;
pub mod process;
pub mod scratch;
