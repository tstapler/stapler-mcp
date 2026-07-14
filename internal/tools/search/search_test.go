package search

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/tstapler/stapler-mcp/internal/tooltypes"
)

func TestClient_Search_ParsesResultsAndSendsAuthHeader(t *testing.T) {
	var gotToken, gotQuery, gotCount string

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotToken = r.Header.Get("X-Subscription-Token")
		gotQuery = r.URL.Query().Get("q")
		gotCount = r.URL.Query().Get("count")

		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"web": map[string]any{
				"results": []map[string]string{
					{"title": "Example Domain", "url": "https://example.com", "description": "an example"},
				},
			},
		})
	}))
	defer srv.Close()

	c := &Client{APIKey: "test-key", BaseURL: srv.URL, HTTPClient: srv.Client()}

	out, err := c.Search(context.Background(), tooltypes.BraveSearchInput{Query: "golang mcp", Count: 5})
	if err != nil {
		t.Fatalf("Search returned error: %v", err)
	}

	if gotToken != "test-key" {
		t.Errorf("X-Subscription-Token = %q, want %q", gotToken, "test-key")
	}
	if gotQuery != "golang mcp" {
		t.Errorf("q param = %q, want %q", gotQuery, "golang mcp")
	}
	if gotCount != "5" {
		t.Errorf("count param = %q, want %q", gotCount, "5")
	}

	if len(out.Results) != 1 || out.Results[0].URL != "https://example.com" {
		t.Fatalf("unexpected results: %+v", out.Results)
	}
}

func TestClient_Search_ErrorsWithoutAPIKey(t *testing.T) {
	c := &Client{APIKey: "", BaseURL: "http://unused.invalid", HTTPClient: http.DefaultClient}

	if _, err := c.Search(context.Background(), tooltypes.BraveSearchInput{Query: "x"}); err == nil {
		t.Fatal("expected error when BRAVE_API_KEY is unset, got nil")
	}
}

func TestClient_Search_CapsCountAtTwenty(t *testing.T) {
	var gotCount string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotCount = r.URL.Query().Get("count")
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"web": map[string]any{"results": []map[string]string{}}})
	}))
	defer srv.Close()

	c := &Client{APIKey: "k", BaseURL: srv.URL, HTTPClient: srv.Client()}
	if _, err := c.Search(context.Background(), tooltypes.BraveSearchInput{Query: "x", Count: 999}); err != nil {
		t.Fatalf("Search returned error: %v", err)
	}
	if gotCount != "20" {
		t.Errorf("count param = %q, want capped %q", gotCount, "20")
	}
}
