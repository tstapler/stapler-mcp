package ipc

import "encoding/json"

// Request is one tool invocation sent from a thin client to the daemon
// over the Unix socket. Each connection carries exactly one Request
// followed by exactly one Response, newline-delimited JSON.
type Request struct {
	// Tool is the name of the daemon-side handler to invoke, e.g.
	// "fetch_page", "brave_web_search", or the built-in "ping"/"shutdown".
	Tool string `json:"tool"`
	// Params is the tool's input, opaque to the transport layer — each
	// handler unmarshals it into its own typed input struct.
	Params json.RawMessage `json:"params,omitempty"`
}

// Response is the daemon's reply to a Request. Exactly one of Result or
// Error is set.
type Response struct {
	Result json.RawMessage `json:"result,omitempty"`
	Error  string          `json:"error,omitempty"`
}

// PingTool is a built-in daemon handler used by clients to check
// liveness without invoking any real tool logic.
const PingTool = "ping"

// ShutdownTool is a built-in daemon handler that asks the daemon to stop
// accepting connections and exit. Used by tests for clean teardown.
const ShutdownTool = "shutdown"
