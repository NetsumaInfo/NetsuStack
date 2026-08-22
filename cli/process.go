package main

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"

	"github.com/creack/pty"
)

type childProcess struct {
	cmd  *exec.Cmd
	pty  *os.File
	pgid int
}

func loginShell(command string) (string, []string) {
	if sh := os.Getenv("SHELL"); filepath.IsAbs(sh) {
		if info, err := os.Stat(sh); err == nil && !info.IsDir() && info.Mode()&0o111 != 0 {
			return sh, []string{"-lc", command}
		}
	}
	if info, err := os.Stat("/bin/bash"); err == nil && !info.IsDir() {
		return "/bin/bash", []string{"-lc", command}
	}
	return "/bin/sh", []string{"-lc", command}
}

func startChild(command, dir string, env []string) (*childProcess, io.Reader, error) {
	if child, reader, err := startChildPTY(command, dir, env); err == nil {
		return child, reader, nil
	}
	return startChildPiped(command, dir, env)
}

func newShellCommand(command, dir string, env []string) *exec.Cmd {
	shell, args := loginShell(command)
	cmd := exec.Command(shell, args...)
	cmd.Dir = dir
	cmd.Env = env
	return cmd
}

func startChildPTY(command, dir string, env []string) (*childProcess, io.Reader, error) {
	cmd := newShellCommand(command, dir, env)
	ptmx, err := pty.Start(cmd)
	if err != nil {
		return nil, nil, err
	}
	if cmd.Process == nil {
		_ = ptmx.Close()
		return nil, nil, fmt.Errorf("process did not start")
	}
	return &childProcess{cmd: cmd, pty: ptmx, pgid: cmd.Process.Pid}, ptmx, nil
}

func startChildPiped(command, dir string, env []string) (*childProcess, io.Reader, error) {
	cmd := newShellCommand(command, dir, env)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, nil, err
	}
	cmd.Stderr = cmd.Stdout
	if err := cmd.Start(); err != nil {
		return nil, nil, err
	}
	return &childProcess{cmd: cmd, pgid: cmd.Process.Pid}, stdout, nil
}

func (c *childProcess) signalGroup(sig syscall.Signal) {
	if c == nil || c.pgid <= 0 {
		return
	}
	_ = syscall.Kill(-c.pgid, sig)
	_ = syscall.Kill(c.pgid, sig)
}

func (c *childProcess) wait() error {
	if c == nil || c.cmd == nil {
		return nil
	}
	err := c.cmd.Wait()
	if c.pty != nil {
		_ = c.pty.Close()
	}
	return err
}

func (c *childProcess) pid() int {
	if c == nil || c.cmd == nil || c.cmd.Process == nil {
		return 0
	}
	return c.cmd.Process.Pid
}

func exitCodeFromWait(err error) int {
	if err == nil {
		return 0
	}
	if exit, ok := err.(*exec.ExitError); ok {
		if status, ok := exit.Sys().(syscall.WaitStatus); ok {
			if status.Signaled() {
				return 128 + int(status.Signal())
			}
			return status.ExitStatus()
		}
		return exit.ExitCode()
	}
	return 1
}

func childEnv(base map[string]string, server string, port *int, extra map[string]string) []string {
	envMap := map[string]string{}
	for _, kv := range os.Environ() {
		if i := indexByte([]byte(kv), '='); i > 0 {
			envMap[kv[:i]] = kv[i+1:]
		}
	}
	delete(envMap, "NO_COLOR")
	envMap["TERM"] = "xterm-256color"
	envMap["COLORTERM"] = "truecolor"
	envMap["FORCE_COLOR"] = "1"
	envMap["CLICOLOR"] = "1"
	envMap["CLICOLOR_FORCE"] = "1"
	envMap["TERM_PROGRAM"] = "Portly"
	envMap["PORTLY"] = "1"
	envMap["PORTLY_SERVER"] = server
	if port != nil {
		envMap["PORT"] = fmt.Sprintf("%d", *port)
	}
	for k, v := range extra {
		envMap[k] = v
	}
	out := make([]string, 0, len(envMap))
	for k, v := range envMap {
		out = append(out, k+"="+v)
	}
	return out
}
