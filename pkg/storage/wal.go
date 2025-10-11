package storage

import (
	"fmt"
	"os"
	"sync"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

type NopWAL struct{}

func NewNopWAL() *NopWAL          { return &NopWAL{} }
func (w *NopWAL) Append(_ string) {}

type FileWAL struct {
	mu sync.Mutex
	f  *os.File
}

func NewFileWAL(path string) (*FileWAL, error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}
	return &FileWAL{f: f}, nil
}
func (w *FileWAL) Append(line string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	fmt.Fprintln(w.f, line)
}

var _ consensus.WAL = (*NopWAL)(nil)
var _ consensus.WAL = (*FileWAL)(nil)
