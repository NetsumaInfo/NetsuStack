package main

import (
	"bytes"
	"encoding/json"
	"time"
)

func encodeJSON(v any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSpace(buf.Bytes()), nil
}

func decodeJSON(data []byte, dest any) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	return dec.Decode(dest)
}

type isoTime struct{ time.Time }

func (t isoTime) MarshalJSON() ([]byte, error) {
	if t.Time.IsZero() {
		return []byte("null"), nil
	}
	return json.Marshal(t.UTC().Format(time.RFC3339))
}

func (t *isoTime) UnmarshalJSON(data []byte) error {
	if string(data) == "null" {
		t.Time = time.Time{}
		return nil
	}
	var s string
	if err := json.Unmarshal(data, &s); err != nil {
		return err
	}
	parsed, err := time.Parse(time.RFC3339, s)
	if err != nil {
		parsed, err = time.Parse(time.RFC3339Nano, s)
	}
	if err != nil {
		return err
	}
	t.Time = parsed
	return nil
}

func ptrTime(t time.Time) *isoTime {
	if t.IsZero() {
		return nil
	}
	return &isoTime{t}
}
