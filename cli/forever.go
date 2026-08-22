package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

const foreverUnitName = "portly.service"

type foreverState struct {
	Enabled          bool   `json:"enabled"`
	Loaded           bool   `json:"loaded"`
	Label            string `json:"label"`
	Unit             string `json:"unit"`
	DaemonExecutable string `json:"daemonExecutable"`
}

func foreverUnitContents(exe string) string {
	return fmt.Sprintf(`[Unit]
Description=Portly headless supervisor
After=default.target

[Service]
Type=simple
ExecStart=%s daemon
Restart=on-failure
WorkingDirectory=%%h

[Install]
WantedBy=default.target
`, exe)
}

func foreverUnitPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".config", "systemd", "user", foreverUnitName)
}

func systemdAvailable() bool {
	_, err := exec.LookPath("systemctl")
	if err != nil {
		return false
	}
	out, err := exec.Command("systemctl", "--user", "--version").CombinedOutput()
	return err == nil && strings.Contains(strings.ToLower(string(out)), "systemd")
}

func currentForeverState() foreverState {
	exe, _ := os.Executable()
	if resolved, err := filepath.EvalSymlinks(exe); err == nil {
		exe = resolved
	}
	st := foreverState{
		Enabled:          fileExists(foreverUnitPath()),
		Label:            foreverUnitName,
		Unit:             foreverUnitPath(),
		DaemonExecutable: exe,
	}
	if systemdAvailable() {
		cmd := exec.Command("systemctl", "--user", "is-active", foreverUnitName)
		if err := cmd.Run(); err == nil {
			st.Loaded = true
		}
	}
	return st
}

func foreverEnable(c *portlyClient) (foreverState, error) {
	if !systemdAvailable() {
		return foreverState{}, fmt.Errorf("systemd --user is not available on this host; Linux forever mode requires a user systemd unit")
	}
	exe, err := os.Executable()
	if err != nil {
		return foreverState{}, err
	}
	if resolved, err := filepath.EvalSymlinks(exe); err == nil {
		exe = resolved
	}
	active := snapshotActiveServers(c)
	if c.reachable(time.Second) {
		_ = c.request("POST", "quit", map[string]any{}, &actionResponse{}, false)
		waitUntilUnreachable(c)
	}
	unit := foreverUnitContents(exe)
	path := foreverUnitPath()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return foreverState{}, err
	}
	if err := os.WriteFile(path, []byte(unit), 0o644); err != nil {
		return foreverState{}, err
	}
	if err := runSystemctl("daemon-reload"); err != nil {
		return foreverState{}, err
	}
	if err := runSystemctl("enable", "--now", foreverUnitName); err != nil {
		return foreverState{}, err
	}
	if !waitForAPI(c) {
		return foreverState{}, fmt.Errorf("systemd loaded Portly, but its control API did not become reachable")
	}
	restoreServers(c, active)
	return currentForeverState(), nil
}

func foreverDisable(c *portlyClient) (foreverState, error) {
	if !systemdAvailable() {
		_ = os.Remove(foreverUnitPath())
		return currentForeverState(), nil
	}
	active := snapshotActiveServers(c)
	_ = runSystemctl("disable", "--now", foreverUnitName)
	_ = os.Remove(foreverUnitPath())
	_ = runSystemctl("daemon-reload")
	if len(active) > 0 {
		if err := c.launchDaemonIfNeeded(); err != nil {
			return foreverState{}, fmt.Errorf("Launch at login was disabled, but Portly could not be relaunched")
		}
		restoreServers(c, active)
	}
	return currentForeverState(), nil
}

func snapshotActiveServers(c *portlyClient) []string {
	if !c.reachable(time.Second) {
		return nil
	}
	var status PortlyStatus
	if err := c.request("GET", "status", nil, &status, false); err != nil {
		return nil
	}
	var ids []string
	for _, project := range status.Projects {
		for _, server := range project.Servers {
			if server.State != StateStopped && server.State != StateFailed {
				ids = append(ids, server.ID)
			}
		}
	}
	return ids
}

func restoreServers(c *portlyClient, ids []string) {
	for _, id := range ids {
		_ = c.post("start", targetRequest{Server: &id}, &actionResponse{})
	}
}

func waitUntilUnreachable(c *portlyClient) {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) && c.reachable(300*time.Millisecond) {
		time.Sleep(200 * time.Millisecond)
	}
}

func waitForAPI(c *portlyClient) bool {
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		if c.reachable(500 * time.Millisecond) {
			return true
		}
		time.Sleep(300 * time.Millisecond)
	}
	return false
}

func runSystemctl(args ...string) error {
	cmd := exec.Command("systemctl", append([]string{"--user"}, args...)...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("systemctl %s failed: %s", strings.Join(args, " "), strings.TrimSpace(string(out)))
	}
	return nil
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}
