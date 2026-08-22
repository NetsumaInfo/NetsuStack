package main

import (
	"net"
	"net/http"
	"strconv"
	"strings"
	"time"
)

func tcpReachable(port int, timeout time.Duration) bool {
	if timeout <= 0 {
		timeout = 2 * time.Second
	}
	targets := []string{
		net.JoinHostPort("127.0.0.1", strconv.Itoa(port)),
		net.JoinHostPort("::1", strconv.Itoa(port)),
		net.JoinHostPort("localhost", strconv.Itoa(port)),
	}
	for _, addr := range targets {
		conn, err := net.DialTimeout("tcp", addr, timeout)
		if err == nil {
			_ = conn.Close()
			return true
		}
	}
	return false
}

func resolvedHealthURL(server ServerConfig) string {
	if server.HealthURL == nil {
		return ""
	}
	raw := strings.TrimSpace(*server.HealthURL)
	if raw == "" {
		return ""
	}
	if strings.HasPrefix(raw, "http://") || strings.HasPrefix(raw, "https://") {
		return raw
	}
	if server.Port == nil {
		return ""
	}
	if !strings.HasPrefix(raw, "/") {
		raw = "/" + raw
	}
	return "http://localhost:" + strconv.Itoa(*server.Port) + raw
}

func httpHealthy(url string, expected *int, timeout time.Duration) bool {
	if timeout <= 0 {
		timeout = 5 * time.Second
	}
	client := &http.Client{Timeout: timeout}
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return false
	}
	req.Header.Set("Cache-Control", "no-cache")
	resp, err := client.Do(req)
	if err != nil {
		return false
	}
	_ = resp.Body.Close()
	if expected != nil {
		return resp.StatusCode == *expected
	}
	return resp.StatusCode >= 200 && resp.StatusCode < 400
}

func checkHealth(server ServerConfig) bool {
	if server.Port != nil {
		if !tcpReachable(*server.Port, 2*time.Second) {
			return false
		}
	}
	if url := resolvedHealthURL(server); url != "" {
		return httpHealthy(url, server.HealthStatus, 5*time.Second)
	}
	return true
}
