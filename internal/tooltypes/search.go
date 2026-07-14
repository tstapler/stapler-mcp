package tooltypes

// BraveSearchInput is the input for the brave_web_search tool.
type BraveSearchInput struct {
	Query string `json:"query" jsonschema:"the search query"`
	// Count caps the number of results returned. Defaults to 10 if zero,
	// capped at 20 (Brave's own per-request maximum).
	Count int `json:"count,omitempty" jsonschema:"number of results to return, defaults to 10, max 20"`
}

// BraveSearchResult is a single organic web result.
type BraveSearchResult struct {
	Title       string `json:"title"`
	URL         string `json:"url"`
	Description string `json:"description"`
}

// BraveSearchOutput is the result of a brave_web_search call.
type BraveSearchOutput struct {
	Results []BraveSearchResult `json:"results"`
}
