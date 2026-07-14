package daemon

import (
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

// acquireExclusiveLock takes a non-blocking exclusive flock on path,
// creating the file if needed. It is the single-instance guarantee: if
// several thin clients race to auto-start the daemon, every spawned
// process calls this, and only the one that wins the flock proceeds to
// bind the socket — the rest see ErrAlreadyRunning and exit immediately.
//
// The returned file must be kept open (not closed) for the lifetime of
// the daemon; the lock is released when the process exits or the file is
// closed.
func acquireExclusiveLock(path string) (*os.File, error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open lockfile: %w", err)
	}

	if err := unix.Flock(int(f.Fd()), unix.LOCK_EX|unix.LOCK_NB); err != nil {
		f.Close()
		if err == unix.EWOULDBLOCK {
			return nil, ErrAlreadyRunning
		}
		return nil, fmt.Errorf("flock: %w", err)
	}

	// Record our PID for operators debugging a stuck daemon; best-effort.
	_ = f.Truncate(0)
	_, _ = f.WriteAt([]byte(fmt.Sprintf("%d\n", os.Getpid())), 0)

	return f, nil
}
