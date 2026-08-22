package main

import "os"

func main() {
	defer func() {
		if rec := recover(); rec != nil {
			if code, ok := rec.(quitCode); ok {
				os.Exit(int(code))
				return
			}
			panic(rec)
		}
	}()
	os.Exit(runCLI(os.Args[1:]))
}
