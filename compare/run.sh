#!/bin/sh
# Benchmarks the C implementation against riffdb-rust inside the compare image.
# Each (threads, scenario, round) starts a fresh server with an empty database;
# C and Rust alternate so drift in the machine affects both equally.
#
# Environment knobs (docker run -e ...):
#   THREADS    worker counts to test          (default "1 4")
#   SCENARIOS  bench scenarios                (default "health query_point query_50 query_1000 execute")
#   ROUNDS     alternating rounds per cell    (default 3)
#   CONNS      keep-alive client connections  (default 8)
#   SECS       seconds per measurement        (default 3)
THREADS=${THREADS:-"1 4"}
SCENARIOS=${SCENARIOS:-"health query_point query_50 query_1000 execute"}
ROUNDS=${ROUNDS:-3}
CONNS=${CONNS:-8}
SECS=${SECS:-3}

C_BIN=/c/build/riffdb
RUST_BIN=/rust/target/release/riffdb
BENCH=/rust/target/release/examples/bench
PORT=19990
RAW=/tmp/raw.txt
: > "$RAW"

echo "C original:"; sed 's/^/  /' /c/REVISIONS
echo "cpus: $(nproc)   conns: $CONNS   secs: $SECS   rounds: $ROUNDS"
echo

run_one() { # impl bin threads scenario
    dir=$(mktemp -d)
    "$2" -p "$PORT" -t "$3" -d "$dir" >"$dir/server.log" 2>&1 &
    pid=$!
    i=0
    until curl -s -o /dev/null "http://127.0.0.1:$PORT/health"; do
        i=$((i + 1)); [ "$i" -gt 100 ] && break; sleep 0.1
    done
    out=$("$BENCH" "$PORT" "$CONNS" "$SECS" "$4" 2>&1)
    if kill -0 "$pid" 2>/dev/null; then alive=yes; else alive=CRASHED; fi
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    line=$(echo "$out" | grep "^$4 ")
    if [ -z "$line" ]; then
        # bench itself failed (e.g. setup could not complete)
        echo "$1 t=$3 $4: bench failed, server $alive: $(echo "$out" | tail -2 | tr '\n' ' ')"
        echo "$1 $3 $4 0 0 0 -1 $alive" >> "$RAW"
    else
        echo "$1 t=$3 $line   server $alive"
        echo "$out" | grep "first error" | sed 's/^ */      /'
        # impl threads scenario rps p50 p99 errors alive
        echo "$line" | awk -v i="$1" -v t="$3" -v a="$alive" \
            '{ gsub("us","",$5); gsub("us","",$7); print i, t, $1, $2, $5, $7, $12, a }' >> "$RAW"
    fi
    rm -rf "$dir"
    sleep 0.5 # let TIME_WAIT sockets and the port settle
}

for t in $THREADS; do
    for sc in $SCENARIOS; do
        r=1
        while [ "$r" -le "$ROUNDS" ]; do
            run_one c    "$C_BIN"    "$t" "$sc"
            run_one rust "$RUST_BIN" "$t" "$sc"
            r=$((r + 1))
        done
    done
done

echo
echo "Summary (mean req/s over rounds; p50/p99 are medians, in us; errors summed):"
echo
awk '
{
    k = $2 " " $3; ks[k] = 1; n[k, $1]++
    rps[k, $1] += $4; e[k, $1] += ($7 < 0 ? 0 : $7)
    p50[k, $1, n[k, $1]] = $5; p99[k, $1, n[k, $1]] = $6
    if ($8 != "yes") crash[k, $1]++
    if ($7 < 0) fail[k, $1]++
}
function med(arr, k, im, cnt,   i, j, v, tmp) {
    for (i = 1; i <= cnt; i++) v[i] = arr[k, im, i]
    for (i = 1; i <= cnt; i++) for (j = i + 1; j <= cnt; j++) if (v[j] + 0 < v[i] + 0) { tmp = v[i]; v[i] = v[j]; v[j] = tmp }
    return v[int((cnt + 1) / 2)]
}
function cell(k, im,   s) {
    s = sprintf("%9.0f  %6s/%-7s", rps[k, im] / n[k, im], med(p50, k, im, n[k, im]), med(p99, k, im, n[k, im]))
    if (e[k, im]) s = s sprintf(" err=%d", e[k, im])
    if (crash[k, im]) s = s sprintf(" crashed=%d/%d", crash[k, im], n[k, im])
    if (fail[k, im]) s = s sprintf(" nobench=%d", fail[k, im])
    return s
}
END {
    printf "| %-3s | %-12s | %-36s | %-36s | %-6s |\n", "t", "scenario", "C  req/s  p50/p99", "Rust  req/s  p50/p99", "Rust/C"
    printf "|-----|--------------|--------------------------------------|--------------------------------------|--------|\n"
    for (k in ks) order[++m] = k
    for (i = 1; i <= m; i++) {
        split(order[i], p, " ")
        c = rps[order[i], "c"] / n[order[i], "c"]; r = rps[order[i], "rust"] / n[order[i], "rust"]
        printf "| %-3s | %-12s | %-36s | %-36s | %6s |\n", p[1], p[2], cell(order[i], "c"), cell(order[i], "rust"), (crash[order[i], "c"] ? "C died" : c > 0 ? sprintf("%.2fx", r / c) : "n/a")
    }
}' "$RAW" | LC_ALL=C sort -t'|' -k2,2n -k3,3
