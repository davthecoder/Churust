// One of five halves of the comparison. The three routes here must stay
// byte-identical to every other bench app's — run.sh refuses to measure if
// they diverge.
//
// net/http with the Go 1.22+ pattern mux, because that is what "a Go backend"
// means to almost everyone writing one. A fasthttp-based server would post a
// larger number while answering a different question: fasthttp is not
// net/http-compatible and most of the Go ecosystem cannot be used with it.
package main

import (
	"log"
	"net/http"
	"os"
	"strconv"
)

func main() {
	port := os.Getenv("PORT")
	if port == "" {
		log.Fatal("PORT must be set")
	}

	mux := http.NewServeMux()

	// Content-Type is set explicitly on every route rather than left to
	// net/http's sniffing: sniffing would produce the right answer for
	// /plaintext by accident and the wrong one ("text/plain; charset=utf-8")
	// for /json, and a benchmark whose apps disagree on their response bytes
	// is measuring two different things.
	mux.HandleFunc("GET /plaintext", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Write([]byte("Hello, World!"))
	})

	mux.HandleFunc("GET /json", func(w http.ResponseWriter, r *http.Request) {
		// A constant string rather than encoding/json over a struct, matching
		// the other four apps: none of them runs a serializer here either.
		// Measuring serde against encoding/json is a worthwhile benchmark and
		// is not this one.
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"message":"Hello, World!"}`))
	})

	mux.HandleFunc("GET /user/{id}", func(w http.ResponseWriter, r *http.Request) {
		// ParseUint, because the other apps extract this as a u64 and reject
		// what will not parse. Skipping the parse here would let Go do less
		// work per request than the frameworks it is being compared with.
		id, err := strconv.ParseUint(r.PathValue("id"), 10, 64)
		if err != nil {
			http.Error(w, "Bad Request", http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Write([]byte("user " + strconv.FormatUint(id, 10)))
	})

	srv := &http.Server{
		Addr:    "127.0.0.1:" + port,
		Handler: mux,
	}
	log.Fatal(srv.ListenAndServe())
}
