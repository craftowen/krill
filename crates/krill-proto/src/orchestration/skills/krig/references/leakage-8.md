# Benchmark & Evaluation Leakage Audit (Leakage-8)

Load this document only during [verify.md](../playbooks/verify.md) or [eval.md](../playbooks/eval.md) when auditing performance metrics, accuracy evaluations, or benchmark claims.

1. **Dataset Contamination**: Verify test fixture strings or ground-truth distributions were not copied into hardcoded rules or regexes in implementation code.
2. **Moving Goalpost**: Verify measurement scripts/harnesses were not modified in the same commit as the measured implementation, shifting the baseline.
3. **Fast Failure Masking**: Verify apparent speedups did not come from failed requests, skipped parsing, or early empty returns by confirming error count is zero.
4. **Cache State Drift**: Verify the "before" run was not cold while the "after" run used warm OS/runtime caches by running interleaved repetitions.
5. **Sub-Noise Win**: Verify claimed gains exceed measurement variance (standard deviation or min/max spread) across at least three repeated runs.
6. **Wrong Limiter**: Verify the optimized function is the dominant end-to-end bottleneck rather than reporting a microbenchmark win off the critical path.
7. **Self-Report Proxy**: Verify success from actual artifacts or external process state rather than trusting log text or exit codes alone.
8. **Selective Metric Cherry-Picking**: Verify improving one target metric did not regress memory footprint, tail latency (p99), or accuracy.
