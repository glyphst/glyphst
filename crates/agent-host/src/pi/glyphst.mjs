// Glyphst Reader's per-run extension for Pi: the Reader's MCP tools, and no
// file writes outside the document's working folder.
import { realpathSync } from "node:fs";
import { isAbsolute, relative, resolve } from "node:path";

export default function (pi) {
  const folder = realpathSync(process.env.GLYPHST_SCRATCH);
  pi.registerMcpServer("glyphst", {
    url: process.env.GLYPHST_MCP_URL,
    headers: { Authorization: `Bearer ${process.env.GLYPHST_MCP_TOKEN}` },
    exposure: "direct",
    description: "Glyphst Reader: the documents the user has open, their pages, annotations and library",
  });
  pi.on("tool_call", async (event) => {
    if (event.toolName !== "write" && event.toolName !== "edit") {
      return undefined;
    }
    const path = resolve(folder, String(event.input?.path ?? ""));
    const inside = relative(folder, path);
    if (inside.startsWith("..") || isAbsolute(inside)) {
      return {
        block: true,
        reason: `Glyphst keeps this agent's files in its working folder ${folder}; write there instead of ${path}.`,
      };
    }
    return undefined;
  });
}
