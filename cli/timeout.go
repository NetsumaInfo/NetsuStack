package main

import (
	"fmt"
	"math"
	"strconv"
	"strings"
)

const (
	defaultTimeoutSeconds = 30 * 60
	maximumTimeoutSeconds = 7 * 24 * 60 * 60
)

func parseTimeout(raw string) (int, bool) {
	value := strings.ToLower(strings.TrimSpace(raw))
	if value == "" {
		return 0, false
	}
	multiplier := 1.0
	number := value
	switch value[len(value)-1] {
	case 's':
		multiplier = 1
		number = value[:len(value)-1]
	case 'm':
		multiplier = 60
		number = value[:len(value)-1]
	case 'h':
		multiplier = 3600
		number = value[:len(value)-1]
	}
	amount, err := strconv.ParseFloat(number, 64)
	if err != nil || amount <= 0 {
		return 0, false
	}
	seconds := int(math.Ceil(amount * multiplier))
	if seconds > maximumTimeoutSeconds {
		return 0, false
	}
	return seconds, true
}

func displayTimeout(seconds int) string {
	if seconds%3600 == 0 {
		return fmt.Sprintf("%dh", seconds/3600)
	}
	if seconds%60 == 0 {
		return fmt.Sprintf("%dm", seconds/60)
	}
	return fmt.Sprintf("%ds", seconds)
}
