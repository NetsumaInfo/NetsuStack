package main

import (
	"crypto/rand"
	"encoding/hex"
	"os"
	"path/filepath"
	"time"
)

func nowTime() time.Time { return time.Now() }

func homeDir() string {
	if override := os.Getenv("PORTLY_HOME"); override != "" {
		return override
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "."
	}
	return filepath.Join(home, ".config", "portly")
}

func configFile() string { return filepath.Join(homeDir(), "config.json") }
func logsDir() string    { return filepath.Join(homeDir(), "logs") }
func logFile(id string) string {
	return filepath.Join(logsDir(), id+".log")
}
func daemonPIDFile() string { return filepath.Join(homeDir(), "daemon.pid") }
func daemonLogFile() string { return filepath.Join(logsDir(), "portly-daemon.log") }

func ensureDirs() error {
	if err := os.MkdirAll(homeDir(), 0o755); err != nil {
		return err
	}
	return os.MkdirAll(logsDir(), 0o755)
}

func newID(prefix string) string {
	var b [4]byte
	_, _ = rand.Read(b[:])
	return prefix + "_" + hex.EncodeToString(b[:])
}

func expandPath(path string) string {
	if path == "" {
		return path
	}
	if path == "~" || (len(path) >= 2 && path[0] == '~' && (path[1] == '/' || path[1] == filepath.Separator)) {
		home, err := os.UserHomeDir()
		if err == nil {
			if path == "~" {
				return home
			}
			return filepath.Join(home, path[2:])
		}
	}
	if abs, err := filepath.Abs(path); err == nil {
		return abs
	}
	return path
}
