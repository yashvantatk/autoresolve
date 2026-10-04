# AutoResolve seeded-bug benchmark

22 tiny Python cases: 10 with one seeded bug, 3 with three bugs each (they measure recall), 9 clean controls (they measure false alarms). Each case has a hidden oracle test
that decides whether the code is really correct, so scores never depend on what the AI claims.

    python3 bench/run_bench.py selfcheck                    # are the cases themselves sound? (no models, free)
    python3 bench/run_bench.py run --label gemini-split     # run every case (uses model quota)
    python3 bench/run_bench.py run --label try --cases slice_last_n wrong_operator --max-calls 150
    python3 bench/run_bench.py compare                      # side-by-side table of all result files

Needs `cargo build -p autoresolve-cli` first (the runner calls target/debug/autoresolve-cli) and your usual
`source ~/autoresolve.env` plus GEMINI_API_KEY. Results land in `bench/results/<label>-<time>.jsonl`.

Budget: about 30 model calls per bug case, about 10 per clean control: roughly 400 calls for the full suite.
The runner stops by itself when a model's daily quota is exhausted (`--max-calls` stops it earlier).

Metrics: see the top of run_bench.py. The two that matter most: **strict pass@1** (a fix backed by a
failing-then-passing regression test AND accepted by the hidden oracle) and **false trust** (a proven fix the
oracle rejects; must stay 0).
