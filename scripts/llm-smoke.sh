#!/usr/bin/env bash
# 本物の Anthropic API との往復を手元で 1 回試す（CI では動かさない。費用がかかる）。
# PoC4（M5 T1）の C の試作は T8 で Conformance に置き換えた。実プロバイダ（`wys llm-proxy` をランタイムから起動する）を
# つなぐ M5 T11 で、本言語のプログラムを使う形に書き直す。
echo "scripts/llm-smoke.sh: the real-provider smoke test is being reworked in M5 T11 (see docs/plans/m5.md)" >&2
exit 1
