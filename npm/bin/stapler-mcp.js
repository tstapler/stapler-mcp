#!/usr/bin/env node

const wasm = require("../pkg/stapler_mcp_wasm.js");
const pkgInfo = require("../package.json");

async function main() {
    if (process.argv.includes("--daemon")) {
        await wasm.run_daemon();
        return;
    }
    await runThinClient();
}

async function runThinClient() {
    const { Server } = require("@modelcontextprotocol/sdk/server");
    const { StdioServerTransport } = require("@modelcontextprotocol/sdk/server/stdio.js");
    const { ListToolsRequestSchema, CallToolRequestSchema } = require("@modelcontextprotocol/sdk/types.js");

    // Same shared schema (via `schemars` on the Rust core types) the native
    // `rmcp` registration uses — not hand-authored a second time here.
    const tools = JSON.parse(wasm.list_tools_json());

    const server = new Server(
        { name: "stapler-mcp", version: pkgInfo.version },
        { capabilities: { tools: {} } },
    );

    server.setRequestHandler(ListToolsRequestSchema, async () => ({
        tools: tools.map((t) => ({
            name: t.name,
            description: t.description,
            inputSchema: t.inputSchema,
            outputSchema: t.outputSchema,
        })),
    }));

    server.setRequestHandler(CallToolRequestSchema, async (request) => {
        const { name, arguments: args } = request.params;
        try {
            const resultJson = await wasm.ensure_daemon_and_call(
                name,
                JSON.stringify(args || {}),
                __filename,
            );
            return {
                content: [{ type: "text", text: resultJson }],
                structuredContent: JSON.parse(resultJson),
                isError: false,
            };
        } catch (e) {
            return {
                content: [{ type: "text", text: e && e.message ? e.message : String(e) }],
                isError: true,
            };
        }
    });

    const transport = new StdioServerTransport();
    await server.connect(transport);
}

main().catch((e) => {
    console.error("stapler-mcp:", e);
    process.exit(1);
});
