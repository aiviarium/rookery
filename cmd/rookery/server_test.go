package main

import (
	"net/url"
	"strings"
	"testing"
)

func TestConfirm(t *testing.T) {
	t.Parallel()

	// Made at run time, the way a configuration carries it.
	databaseURL := (&url.URL{
		Scheme:   "postgres",
		User:     url.UserPassword("rookery", "not-a-real-password"),
		Host:     "127.0.0.1:55432",
		Path:     "/rookery",
		RawQuery: "sslmode=disable",
	}).String()

	cases := []struct {
		name    string
		answer  string
		wantErr bool
	}{
		{name: "reset", answer: "reset\n"},
		{name: "reset without a newline", answer: "reset"},
		{name: "reset with spaces", answer: "  reset  \n"},
		{name: "yes", answer: "yes\n", wantErr: true},
		{name: "nothing", answer: "\n", wantErr: true},
		{name: "nothing at all", answer: "", wantErr: true},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			var out strings.Builder
			err := confirm(strings.NewReader(tc.answer), &out, databaseURL)
			if tc.wantErr {
				if err == nil {
					t.Fatal("confirm accepted an answer other than reset")
				}
				return
			}
			if err != nil {
				t.Fatalf("confirm: %v", err)
			}

			prompt := out.String()
			if !strings.Contains(prompt, "127.0.0.1:55432/rookery") {
				t.Errorf("the prompt does not say which database: %q", prompt)
			}
			if strings.Contains(prompt, "not-a-real-password") {
				t.Errorf("the prompt carries the password: %q", prompt)
			}
		})
	}
}
