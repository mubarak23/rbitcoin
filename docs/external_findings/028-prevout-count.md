# 028 — Script job with no prevouts succeeds

**Severity:** high
**Status:** fixed
**Found by:** coordinated review, 2026-09-22

`verify_job_all_inputs` returned success when `prevouts` was empty, including
when the transaction had inputs. A length mismatch is now an error. A
matching length still verifies.

**Regression:** `rbitcoin-consensus`
`script::verify_routing_tests::script_job_build_rejects_unusable_prevouts`.
Since aeb5c17dc the check is in `ScriptCheckJob::try_from_parts`, so a job
with the wrong prevout count is never built.
