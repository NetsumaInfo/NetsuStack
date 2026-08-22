package main

import (
	"fmt"
	"math"
	"strconv"
	"strings"
)

const (
	minMemoryLimitBytes uint64 = 128 * 1024 * 1024
	maxMemoryLimitBytes uint64 = 1024 * 1024 * 1024 * 1024
	gibibyte            uint64 = 1024 * 1024 * 1024
	mebibyte            uint64 = 1024 * 1024
)

func parseMemorySize(raw string) (uint64, bool) {
	normalized := strings.ToLower(strings.ReplaceAll(strings.ReplaceAll(strings.TrimSpace(raw), ",", "."), " ", ""))
	if normalized == "" {
		return 0, false
	}
	units := []struct {
		suffix     string
		multiplier float64
	}{
		{"tib", 1099511627776}, {"tb", 1099511627776}, {"to", 1099511627776},
		{"gib", 1073741824}, {"gb", 1073741824}, {"go", 1073741824},
		{"mib", 1048576}, {"mb", 1048576}, {"mo", 1048576},
	}
	var multiplier float64
	number := normalized
	found := false
	for _, unit := range units {
		if strings.HasSuffix(normalized, unit.suffix) {
			number = strings.TrimSuffix(normalized, unit.suffix)
			multiplier = unit.multiplier
			found = true
			break
		}
	}
	if !found {
		return 0, false
	}
	amount, err := strconv.ParseFloat(number, 64)
	if err != nil || amount <= 0 {
		return 0, false
	}
	bytes := amount * multiplier
	if !isFinite(bytes) || bytes < float64(minMemoryLimitBytes) || bytes > float64(maxMemoryLimitBytes) {
		return 0, false
	}
	return uint64(math.Round(bytes)), true
}

func displayMemorySize(bytes uint64) string {
	if bytes >= gibibyte {
		value := float64(bytes) / float64(gibibyte)
		if value == math.Trunc(value) {
			return fmt.Sprintf("%.0f GB", value)
		}
		return fmt.Sprintf("%.1f GB", value)
	}
	return fmt.Sprintf("%d MB", bytes/mebibyte)
}

func isFinite(v float64) bool {
	return !math.IsNaN(v) && !math.IsInf(v, 0)
}

func validMemoryLimit(bytes *uint64) bool {
	if bytes == nil {
		return false
	}
	return *bytes >= minMemoryLimitBytes && *bytes <= maxMemoryLimitBytes
}

type memoryGuard struct {
	overLimit map[string]int
}

const requiredConsecutiveSamples = 3

func newMemoryGuard() *memoryGuard {
	return &memoryGuard{overLimit: map[string]int{}}
}

func (g *memoryGuard) shouldRestart(projectID string, footprint, limit uint64, hasRunning bool) bool {
	if limit == 0 || !hasRunning {
		g.overLimit[projectID] = 0
		return false
	}
	if footprint <= limit {
		g.overLimit[projectID] = 0
		return false
	}
	g.overLimit[projectID]++
	if g.overLimit[projectID] < requiredConsecutiveSamples {
		return false
	}
	g.overLimit[projectID] = 0
	return true
}

func (g *memoryGuard) reset(projectID string) { delete(g.overLimit, projectID) }
func (g *memoryGuard) resetAll()              { g.overLimit = map[string]int{} }
