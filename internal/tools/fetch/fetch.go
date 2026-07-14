// Package fetch implements the fetch_page tool: render a URL in a
// headless browser (via chromedp — pure Go, no Node/Playwright driver)
// and return its extracted title and text content. This lives daemon-side
// because it's the canonical example of state worth sharing across a
// whole machine's worth of thin clients: chromedp's browser allocator
// pool is exactly the kind of heavyweight resource that must not be
// duplicated per-subagent.
//
// The same chromedp dependency established here is intended to later
// back a P1 browser-automation tool set (playwright-mcp equivalent) —
// see NOTES.md.
package fetch

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/chromedp/chromedp"
	"github.com/tstapler/stapler-mcp/internal/tooltypes"
)

const defaultTimeout = 30 * time.Second

// Fetcher renders pages in a headless browser. It is safe for concurrent
// use — each Fetch call gets its own chromedp tab context, but they all
// share one underlying allocator/browser process, which is the whole
// point of running this daemon-side instead of per-session.
type Fetcher struct {
	allocCtx context.Context
	cancel   context.CancelFunc
}

// NewFetcher starts the shared headless browser allocator. Call Close
// when the daemon shuts down.
func NewFetcher() *Fetcher {
	allocCtx, cancel := chromedp.NewExecAllocator(context.Background(), chromedp.DefaultExecAllocatorOptions[:]...)
	return &Fetcher{allocCtx: allocCtx, cancel: cancel}
}

// Close releases the shared browser allocator.
func (f *Fetcher) Close() {
	f.cancel()
}

// Fetch renders in.URL and extracts its title and visible text.
func (f *Fetcher) Fetch(ctx context.Context, in tooltypes.FetchPageInput) (tooltypes.FetchPageOutput, error) {
	var out tooltypes.FetchPageOutput

	if in.URL == "" {
		return out, fmt.Errorf("url must not be empty")
	}

	timeout := defaultTimeout
	if in.TimeoutSeconds > 0 {
		timeout = time.Duration(in.TimeoutSeconds) * time.Second
	}

	tabCtx, cancelTab := chromedp.NewContext(f.allocCtx)
	defer cancelTab()
	tabCtx, cancelTimeout := context.WithTimeout(tabCtx, timeout)
	defer cancelTimeout()

	var title, text, html, finalURL string
	err := chromedp.Run(tabCtx,
		chromedp.Navigate(in.URL),
		chromedp.Title(&title),
		chromedp.Location(&finalURL),
		chromedp.OuterHTML("html", &html, chromedp.ByQuery),
		chromedp.Text("body", &text, chromedp.ByQuery, chromedp.NodeVisible),
	)
	if err != nil {
		return out, fmt.Errorf("render %s: %w", in.URL, err)
	}

	out.Title = title
	out.Text = text
	out.FinalURL = finalURL

	if in.SavePath != "" {
		if err := os.MkdirAll(filepath.Dir(in.SavePath), 0o755); err != nil {
			return out, fmt.Errorf("create save directory: %w", err)
		}
		if err := os.WriteFile(in.SavePath, []byte(html), 0o644); err != nil {
			return out, fmt.Errorf("save page to %s: %w", in.SavePath, err)
		}
		out.SavedTo = in.SavePath
	}

	return out, nil
}
