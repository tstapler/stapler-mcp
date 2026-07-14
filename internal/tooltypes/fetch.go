// Package tooltypes holds the input/output structs shared between the
// thin MCP-facing tool schemas (internal/mcpserver) and the daemon-side
// tool implementations (internal/tools/...). Keeping them in one place
// means both sides serialize/deserialize the exact same shape over the
// daemon IPC boundary.
package tooltypes

// FetchPageInput is the input for the fetch_page tool: render a URL in a
// headless browser and return its extracted content.
type FetchPageInput struct {
	URL string `json:"url" jsonschema:"the URL to fetch and render"`
	// SavePath, if set, also writes the rendered HTML to this local path
	// (mirrors mcp-website-downloader's download_page behavior).
	SavePath string `json:"savePath,omitempty" jsonschema:"optional local file path to save the rendered HTML to"`
	// TimeoutSeconds bounds how long the headless browser is allowed to
	// spend loading the page. Defaults to 30 if zero.
	TimeoutSeconds int `json:"timeoutSeconds,omitempty" jsonschema:"navigation timeout in seconds, defaults to 30"`
}

// FetchPageOutput is the result of a fetch_page call.
type FetchPageOutput struct {
	Title    string `json:"title" jsonschema:"the page's <title>"`
	Text     string `json:"text" jsonschema:"visible text content extracted from the rendered page"`
	SavedTo  string `json:"savedTo,omitempty" jsonschema:"local path the HTML was saved to, if savePath was set"`
	FinalURL string `json:"finalUrl" jsonschema:"the URL after any redirects"`
}
