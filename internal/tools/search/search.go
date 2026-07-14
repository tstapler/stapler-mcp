// Package search implements the brave_web_search tool: a stateless HTTP
// wrapper around the Brave Search API. It runs daemon-side purely for
// architectural consistency with the other tools (every tool call goes
// through the same daemon IPC path) — unlike fetch_page, it holds no
// state worth sharing across calls.
package search

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"

	"github.com/tstapler/stapler-mcp/internal/tooltypes"
)

const defaultBaseURL = "https://api.search.brave.com/res/v1/web/search"

// Client performs Brave Search API requests. The zero value is not
// usable — construct with NewClient. BaseURL and HTTPClient are exported
// so tests can point Client at an httptest server instead of the real API.
type Client struct {
	APIKey     string
	BaseURL    string
	HTTPClient *http.Client
}

// NewClient builds a Client reading BRAVE_API_KEY from the environment.
// It does not error on a missing key — that only surfaces when Search is
// actually called, matching how the other tools fail lazily on use.
func NewClient() *Client {
	return &Client{
		APIKey:     os.Getenv("BRAVE_API_KEY"),
		BaseURL:    defaultBaseURL,
		HTTPClient: http.DefaultClient,
	}
}

// Search calls the Brave Web Search API and returns organic results.
func (c *Client) Search(ctx context.Context, in tooltypes.BraveSearchInput) (tooltypes.BraveSearchOutput, error) {
	var out tooltypes.BraveSearchOutput

	if c.APIKey == "" {
		return out, fmt.Errorf("BRAVE_API_KEY is not set")
	}
	if in.Query == "" {
		return out, fmt.Errorf("query must not be empty")
	}

	count := in.Count
	switch {
	case count <= 0:
		count = 10
	case count > 20:
		count = 20
	}

	q := url.Values{}
	q.Set("q", in.Query)
	q.Set("count", fmt.Sprintf("%d", count))

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.BaseURL+"?"+q.Encode(), nil)
	if err != nil {
		return out, fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("X-Subscription-Token", c.APIKey)

	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return out, fmt.Errorf("brave search request: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return out, fmt.Errorf("read response body: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return out, fmt.Errorf("brave search: HTTP %d: %s", resp.StatusCode, string(body))
	}

	var raw braveResponse
	if err := json.Unmarshal(body, &raw); err != nil {
		return out, fmt.Errorf("decode brave response: %w", err)
	}

	for _, r := range raw.Web.Results {
		out.Results = append(out.Results, tooltypes.BraveSearchResult{
			Title:       r.Title,
			URL:         r.URL,
			Description: r.Description,
		})
	}
	return out, nil
}

// braveResponse is the subset of Brave's web search response shape we
// actually consume.
type braveResponse struct {
	Web struct {
		Results []struct {
			Title       string `json:"title"`
			URL         string `json:"url"`
			Description string `json:"description"`
		} `json:"results"`
	} `json:"web"`
}
