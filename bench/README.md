# AutoResolve seeded-bug benchmark

22 tiny Python cases: 13 with one seeded bug, 3 with three bugs each (9 bugs, they measure recall), and 6 clean controls with no bug (they measure false alarms). Each case has a hidden oracle test
that decides whether the code is really correct, so scores never depend on what the AI claims.

    python3 bench/run_bench.py selfcheck                    # are the cases themselves sound? (no models, free)
    python3 bench/run_bench.py run --label gemini-split     # run every case (uses model quota)
    python3 bench/run_bench.py run --label try --cases slice_last_n wrong_operator --max-calls 150
    python3 bench/run_bench.py compare                      # side-by-side table of all result files

Needs `cargo build -p autoresolve-cli` first (the runner calls target/debug/autoresolve-cli) and your usual
`source ~/autoresolve.env` plus GEMINI_API_KEY. Results land in `bench/results/<label>-<time>.jsonl`.

Budget (measured on the first 15 cases): about 21 model calls per bug case and 4 per clean control, so the first 15 cases cost about 310 calls. The three multi-bug cases cost more (about 60 each). Expect roughly 500 calls for all 22.
The runner stops by itself when a model's daily quota is exhausted (`--max-calls` stops it earlier).

Metrics: see the top of run_bench.py. The two that matter most: **strict pass@1** (a fix backed by a
failing-then-passing regression test AND accepted by the hidden oracle) and **false trust** (a proven fix the
oracle rejects; must stay 0).
