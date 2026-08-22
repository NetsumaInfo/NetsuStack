package main

import (
	"os"
	"sync"
	"time"
)

type configStore struct {
	mu             sync.Mutex
	path           string
	config         PortlyConfig
	onChange       func(PortlyConfig)
	selfWriteUntil time.Time
	lastMod        time.Time
	stopWatch      chan struct{}
}

func newConfigStore(path string) *configStore {
	_ = ensureDirs()
	store := &configStore{path: path, config: defaultConfig(), stopWatch: make(chan struct{})}
	if loaded, ok := readConfig(path); ok {
		store.config = loaded
	} else {
		_ = writeConfig(path, store.config)
	}
	if info, err := os.Stat(path); err == nil {
		store.lastMod = info.ModTime()
	}
	return store
}

func (s *configStore) current() PortlyConfig {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.config
}

func (s *configStore) mutate(fn func(*PortlyConfig)) {
	s.mu.Lock()
	fn(&s.config)
	s.selfWriteUntil = time.Now().Add(time.Second)
	_ = writeConfig(s.path, s.config)
	if info, err := os.Stat(s.path); err == nil {
		s.lastMod = info.ModTime()
	}
	s.mu.Unlock()
}

func (s *configStore) startWatching() {
	go func() {
		ticker := time.NewTicker(400 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-s.stopWatch:
				return
			case <-ticker.C:
				s.reloadIfChanged()
			}
		}
	}()
}

func (s *configStore) stopWatching() {
	select {
	case <-s.stopWatch:
	default:
		close(s.stopWatch)
	}
}

func (s *configStore) reloadIfChanged() {
	info, err := os.Stat(s.path)
	if err != nil {
		return
	}
	s.mu.Lock()
	if !info.ModTime().After(s.lastMod) || time.Now().Before(s.selfWriteUntil) {
		s.mu.Unlock()
		return
	}
	s.lastMod = info.ModTime()
	s.mu.Unlock()
	loaded, ok := readConfig(s.path)
	if !ok {
		return
	}
	s.mu.Lock()
	s.config = loaded
	cb := s.onChange
	s.mu.Unlock()
	if cb != nil {
		cb(loaded)
	}
}

func readConfig(path string) (PortlyConfig, bool) {
	data, err := os.ReadFile(path)
	if err != nil {
		return PortlyConfig{}, false
	}
	var cfg PortlyConfig
	if err := decodeJSON(data, &cfg); err != nil {
		return PortlyConfig{}, false
	}
	return cfg, true
}

func writeConfig(path string, cfg PortlyConfig) error {
	_ = ensureDirs()
	data, err := encodeJSON(cfg)
	if err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, append(data, '\n'), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
