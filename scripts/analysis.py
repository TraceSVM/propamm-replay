#!/usr/bin/env python3

import argparse
import glob
import json
import os
import time
import warnings
from collections import defaultdict

import matplotlib
import numpy as np
import polars as pl

matplotlib.use("Agg")

import matplotlib.pyplot as plt
from scipy import stats as scipy_stats

warnings.filterwarnings("ignore")

SEP = "=" * 60

PROTOCOL_COLORS = {
    "humidifi": "#1f77b4",
    "solfiv2": "#ff7f0e",
    "goonfi": "#2ca02c",
    "zerofi": "#d62728",
    "tesserav": "#9467bd",
    "alphaq": "#8c564b",
    "bisonfi": "#7f7f7f",
    "sol_fi_v2": "#ff7f0e",
    "goon_fi": "#2ca02c",
    "zero_fi": "#d62728",
    "tessera_v": "#9467bd",
    "alpha_q": "#8c564b",
    "bison_fi": "#7f7f7f",
    "orca_clmm": "#e377c2",
    "raydium_clmm": "#bcbd22",
}

PROTOCOL_LABELS = {
    "humidifi": "Humidifi",
    "solfiv2": "SolFi V2",
    "goonfi": "GoonFi V2",
    "zerofi": "ZeroFi",
    "tesserav": "Tessera V",
    "alphaq": "AlphaQ",
    "bisonfi": "BisonFi",
    "sol_fi_v2": "SolFi V2",
    "goon_fi": "GoonFi V2",
    "zero_fi": "ZeroFi",
    "tessera_v": "Tessera V",
    "alpha_q": "AlphaQ",
    "bison_fi": "BisonFi",
    "orca_clmm": "Orca CLMM",
    "raydium_clmm": "Raydium CLMM",
}

SKIP_SWAP_PROTOCOLS = {"tesserav", "tessera_v"}

EXCLUDE_CROSS_COMPARE = {"alphaq", "alpha_q"}

MAX_VAULT = 2**62

PRICE_SCALE = 1e3
SOL_DECIMALS = 9
USDC_DECIMALS = 6

plt.rcParams.update({
    "font.family": "serif",
    "font.size": 10,
    "axes.titlesize": 11,
    "axes.labelsize": 10,
    "xtick.labelsize": 9,
    "ytick.labelsize": 9,
    "legend.fontsize": 8,
    "figure.dpi": 300,
    "savefig.dpi": 300,
    "savefig.bbox": "tight",
    "savefig.pad_inches": 0.05,
})


def load_metadata(session_dir: str) -> dict:
    with open(os.path.join(session_dir, "metadata.json")) as f:
        return json.load(f)


def load_parquets(directory: str, prefix: str) -> pl.DataFrame:
    pattern = os.path.join(directory, prefix + "_*.parquet")
    files = sorted(glob.glob(pattern))
    exact = os.path.join(directory, prefix + ".parquet")
    if os.path.isfile(exact) and exact not in files:
        files.insert(0, exact)
    if not files:
        return pl.DataFrame()
    frames = []
    for fp in files:
        try:
            frames.append(pl.read_parquet(fp))
        except Exception:
            continue
    if not frames:
        return pl.DataFrame()
    return pl.concat(frames)


def load_pool_data(session_dir: str, data_dir: str, kind: str) -> pl.DataFrame:
    pool_dir = os.path.join(session_dir, "pools", data_dir)
    df = load_parquets(pool_dir, kind)
    if df.shape[0] > 0:
        return df

    parts = data_dir.rsplit("_", 1)
    pair = parts[0] if len(parts) == 2 else data_dir
    qo_dir = os.path.join(session_dir, "quoter_output", pair)

    prefix = "derived" if kind == "derived_state" else kind
    df = load_parquets(qo_dir, prefix)

    if df.shape[0] > 0 and "pool_id" in df.columns:
        pool_prefix = parts[1] if len(parts) == 2 else ""
        if pool_prefix:
            df = df.filter(pl.col("pool_id").str.starts_with(pool_prefix))

    return df


def load_pyth(session_dir: str) -> pl.DataFrame:
    pyth_dir = os.path.join(session_dir, "pools", "pyth_prices")
    return load_parquets(pyth_dir, "pyth_prices")


class PythIndex:
    def __init__(self, pyth_df: pl.DataFrame, symbol: str = "SOL/USD"):
        sub = (
            pyth_df
            .filter(pl.col("symbol") == symbol)
            .with_columns(
                (
                    pl.col("price").cast(pl.Float64) * (10.0 ** pl.col("expo").cast(pl.Float64))
                ).alias("usd")
            )
            .sort("timestamp_ms")
        )
        self.ts = sub["timestamp_ms"].to_numpy()
        self.px = sub["usd"].to_numpy()

    def at(self, ts_ms):
        idx = np.searchsorted(self.ts, ts_ms, side="right") - 1
        return self.px[max(0, min(idx, len(self.px) - 1))]

    def vec(self, ts_arr):
        idxs = np.searchsorted(self.ts, ts_arr, side="right") - 1
        idxs = np.clip(idxs, 0, len(self.px) - 1)
        return self.px[idxs]


def detect_swaps(states: pl.DataFrame, skip_rows: int = 5) -> pl.DataFrame:
    bcol, qcol = "base_vault_balance", "quote_vault_balance"
    if bcol not in states.columns or qcol not in states.columns:
        return pl.DataFrame()

    real = (
        states
        .filter(pl.col("slot") > 0)
        .filter(pl.col("txn_signature").is_not_null() & (pl.col("txn_signature") != ""))
        .filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT))
        .filter((pl.col(qcol) > 0) & (pl.col(qcol) < MAX_VAULT))
        .sort(["slot", "write_version"])
    )

    if real.height < 10:
        return pl.DataFrame()

    deduped = (
        real.group_by("txn_signature", maintain_order=True).last().sort(["slot", "write_version"])
    )

    if deduped.height <= skip_rows + 1:
        return pl.DataFrame()

    deduped = deduped.slice(skip_rows)

    deduped = deduped.with_columns([
        pl.col(bcol).cast(pl.Int64).diff().alias("bd"),
        pl.col(qcol).cast(pl.Int64).diff().alias("qd"),
    ]).drop_nulls(subset=["bd"])

    swaps = deduped.filter(
        (pl.col("bd") != 0) & (pl.col("qd") != 0) & ((pl.col("bd") > 0) != (pl.col("qd") > 0))
    )

    if swaps.height == 0:
        return pl.DataFrame()

    swaps = swaps.with_columns(
        pl.when(pl.col("bd") > 0).then(pl.lit("B2Q")).otherwise(pl.lit("Q2B")).alias("direction")
    )

    return swaps


def detect_swaps_for_pool(session_dir: str, pool: dict) -> pl.DataFrame:
    if pool["amm_type"] in SKIP_SWAP_PROTOCOLS:
        return pl.DataFrame()
    states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
    if states.height == 0:
        return pl.DataFrame()
    return detect_swaps(states)


def filter_capital_events(base: np.ndarray, quote: np.ndarray) -> np.ndarray:
    db = np.diff(base)
    dq = np.diff(quote)

    base_only = (np.abs(db) > 0.001) & (np.abs(dq) < 0.001)
    quote_only = (np.abs(dq) > 0.001) & (np.abs(db) < 0.001)
    same_dir = (np.abs(db) > 0.001) & (np.abs(dq) > 0.001) & ((db > 0) == (dq > 0))

    is_capital = base_only | quote_only | same_dir

    segment_ids = np.zeros(len(base), dtype=np.int64)
    seg = 0
    segment_ids[0] = seg
    for i in range(len(db)):
        if is_capital[i]:
            seg += 1
        segment_ids[i + 1] = seg

    return segment_ids, is_capital


def find_primary_pool(pools: list, amm_type: str, session_dir: str | None = None) -> dict | None:
    candidates = [p for p in pools if p["amm_type"] == amm_type]
    if not candidates:
        return None
    if len(candidates) == 1:
        return candidates[0]
    if session_dir is None:
        return candidates[0]
    best, best_n = candidates[0], 0
    for c in candidates:
        df = load_pool_data(session_dir, c["data_dir"], "derived_state")
        if df.height > best_n:
            best, best_n = c, df.height
    return best


def compute_bid_ask(quotes: pl.DataFrame, tier_usd: float = 100.0) -> pl.DataFrame:
    b2q = (
        quotes
        .filter(
            (pl.col("direction") == "B2Q")
            & ((pl.col("input_usd_equiv") - tier_usd).abs() < 0.1)
            & (pl.col("output_amount") > 0)
            & (pl.col("slot") > 0)
        )
        .with_columns(
            (
                pl.col("output_amount").cast(pl.Float64)
                / pl.col("input_amount").cast(pl.Float64)
                * PRICE_SCALE
            ).alias("bid")
        )
        .select(["timestamp_ms", "bid"])
    )

    q2b = (
        quotes
        .filter(
            (pl.col("direction") == "Q2B")
            & ((pl.col("input_usd_equiv") - tier_usd).abs() < 0.1)
            & (pl.col("output_amount") > 0)
            & (pl.col("slot") > 0)
        )
        .with_columns(
            (
                pl.col("input_amount").cast(pl.Float64)
                / pl.col("output_amount").cast(pl.Float64)
                * PRICE_SCALE
            ).alias("ask")
        )
        .select(["timestamp_ms", "ask"])
    )

    paired = b2q.join(q2b, on="timestamp_ms", how="inner").sort("timestamp_ms")
    if paired.height < 2:
        return pl.DataFrame()

    paired = paired.with_columns([
        ((pl.col("bid") + pl.col("ask")) / 2.0).alias("mid"),
        ((pl.col("ask") - pl.col("bid")) / ((pl.col("bid") + pl.col("ask")) / 2.0) * 10000).alias(
            "spread_bps"
        ),
    ])
    return paired


def section_overview(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.1: Dataset Overview\n{SEP}")

    pools = metadata["pools"]
    proto_stats = defaultdict(
        lambda: {
            "n_pools": 0,
            "n_active_pools": 0,
            "total_state_updates": 0,
            "total_swaps": 0,
            "total_volume_usd": 0.0,
            "total_tvl_usd": 0.0,
            "min_ts": float("inf"),
            "max_ts": 0,
        }
    )

    for pool in pools:
        amm = pool["amm_type"]
        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            print(f"  [SKIP] No state data for {pool['pool_id'][:8]} ({amm})")
            continue

        ps = proto_stats[amm]
        ps["n_pools"] += 1
        ps["total_state_updates"] += states.height

        ts_min = states["timestamp_ms"].min()
        ts_max = states["timestamp_ms"].max()
        ps["min_ts"] = min(ps["min_ts"], ts_min)
        ps["max_ts"] = max(ps["max_ts"], ts_max)

        swaps = detect_swaps(states) if amm not in SKIP_SWAP_PROTOCOLS else pl.DataFrame()
        n_swaps = swaps.height
        if n_swaps > 0:
            ps["n_active_pools"] += 1
        ps["total_swaps"] += n_swaps

        if n_swaps > 0:
            ps["total_volume_usd"] += swaps["qd"].abs().sum() / 1e6

        bcol, qcol = "base_vault_balance", "quote_vault_balance"
        if bcol in states.columns and qcol in states.columns:
            last = states.tail(1)
            bv = last[bcol][0]
            qv = last[qcol][0]
            if bv < MAX_VAULT and qv < MAX_VAULT:
                sol_price = pyth_sol.at(ts_max)
                tvl = bv / 1e9 * sol_price + qv / 1e6
                ps["total_tvl_usd"] += tvl

    rows = []
    total = {
        "protocol": "TOTAL",
        "n_pools": 0,
        "n_active_pools": 0,
        "total_state_updates": 0,
        "total_swaps": 0,
        "total_volume_usd": 0.0,
        "total_tvl_usd": 0.0,
        "updates_per_hour": 0.0,
        "duration_hours": 0.0,
    }

    for amm in sorted(proto_stats.keys()):
        ps = proto_stats[amm]
        dur_ms = ps["max_ts"] - ps["min_ts"] if ps["max_ts"] > ps["min_ts"] else 1
        dur_h = dur_ms / 3.6e6
        uph = ps["total_state_updates"] / dur_h if dur_h > 0 else 0

        row = {
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "n_pools": ps["n_pools"],
            "n_active_pools": ps["n_active_pools"],
            "total_state_updates": ps["total_state_updates"],
            "total_swaps": ps["total_swaps"],
            "total_volume_usd": round(ps["total_volume_usd"], 2),
            "total_tvl_usd": round(ps["total_tvl_usd"], 2),
            "updates_per_hour": round(uph, 1),
            "duration_hours": round(dur_h, 2),
        }
        rows.append(row)
        for k in ["n_pools", "n_active_pools", "total_state_updates", "total_swaps"]:
            total[k] += row[k]
        total["total_volume_usd"] += ps["total_volume_usd"]
        total["total_tvl_usd"] += ps["total_tvl_usd"]

    total["total_volume_usd"] = round(total["total_volume_usd"], 2)
    total["total_tvl_usd"] = round(total["total_tvl_usd"], 2)
    rows.append(total)

    df_out = pl.DataFrame(rows)
    path = os.path.join(out_dir, "table_dataset_overview.csv")
    df_out.write_csv(path)
    print(df_out)
    print(f"Saved: {path}")


def section_spreads(
    session_dir: str, metadata: dict, pyth_sol: PythIndex, pyth_df: pl.DataFrame, out_dir: str
):
    _spread_vs_size(session_dir, metadata, out_dir)
    _price_tracking(session_dir, metadata, pyth_sol, pyth_df, out_dir)
    _price_comparison(session_dir, metadata, pyth_sol, out_dir)
    _quote_update_rate(session_dir, metadata, out_dir)
    _spread_tiers(session_dir, metadata, pyth_sol, pyth_df, out_dir)
    _spread_stats_table(session_dir, metadata, out_dir)


def _spread_vs_size(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.2: Spread vs Size\n{SEP}")

    pools = metadata["pools"]

    primary_pools = {}
    for pool in pools:
        amm = pool["amm_type"]
        if amm in EXCLUDE_CROSS_COMPARE:
            continue
        df = load_pool_data(session_dir, pool["data_dir"], "quotes")
        nrows = df.filter(pl.col("slot") > 0).height if df.height > 0 else 0
        if nrows == 0:
            continue
        if amm not in primary_pools or nrows > primary_pools[amm][1]:
            primary_pools[amm] = (pool, nrows)

    fig, ax = plt.subplots(figsize=(6, 4))

    for amm, (pool, _) in sorted(primary_pools.items()):
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        if quotes.height == 0:
            continue

        quotes = quotes.filter(pl.col("slot") > 0)
        sizes = sorted(quotes["input_usd_equiv"].unique().to_list())
        spread_points = []

        for sz in sizes:
            paired = compute_bid_ask(quotes, sz)
            if paired.height < 10:
                continue
            median_spread = paired["spread_bps"].median()
            if median_spread is not None and median_spread > 0:
                spread_points.append((sz, median_spread))

        if spread_points:
            xs, ys = zip(*spread_points)
            ax.plot(
                xs,
                ys,
                marker="o",
                markersize=3,
                linewidth=1.5,
                color=PROTOCOL_COLORS.get(amm, "gray"),
                label=PROTOCOL_LABELS.get(amm, amm),
            )

    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("Trade size (USD)")
    ax.set_ylabel("Two-sided spread (bps)")
    ax.set_title("Effective Spread vs Trade Size")
    ax.legend(fontsize=8)
    ax.grid(True, which="both", alpha=0.3)
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_spread_vs_size.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _price_tracking(
    session_dir: str, metadata: dict, pyth_sol: PythIndex, pyth_df: pl.DataFrame, out_dir: str
):
    print(f"\n{SEP}\nSECTION 7.3.2: Price Tracking\n{SEP}")

    pools = metadata["pools"]
    candidates = [p for p in pools if p["amm_type"] == "humidifi"]
    pool = None
    for c in sorted(
        candidates,
        key=lambda p: load_pool_data(session_dir, p["data_dir"], "derived_state").height,
        reverse=True,
    ):
        states_c = load_pool_data(session_dir, c["data_dir"], "derived_state")
        if states_c.height < 100:
            continue
        b = (
            states_c.filter(pl.col("slot") > 0)["base_vault_balance"].to_numpy().astype(np.float64)
            / 1e9
        )
        db = np.abs(np.diff(b))
        q = (
            states_c.filter(pl.col("slot") > 0)["quote_vault_balance"].to_numpy().astype(np.float64)
            / 1e6
        )
        dq = np.abs(np.diff(q))
        if not ((db > 100) & (dq < 1.0)).any():
            pool = c
            break
    if pool is None and candidates:
        pool = candidates[0]
    if pool is None:
        print("  No Humidifi pool found, skipping")
        return

    label = PROTOCOL_LABELS.get(pool["amm_type"], pool["amm_type"])
    color = PROTOCOL_COLORS.get(pool["amm_type"], "blue")

    quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
    paired = compute_bid_ask(quotes, 100.0)
    if paired.height < 10:
        print("  Insufficient quote data")
        return

    step = max(1, paired.height // 3000)
    paired_sub = paired.gather_every(step)

    ts_arr = paired_sub["timestamp_ms"].to_numpy().astype(np.float64)
    bid_arr = paired_sub["bid"].to_numpy()
    ask_arr = paired_sub["ask"].to_numpy()
    mid_arr = paired_sub["mid"].to_numpy()

    t0 = ts_arr[0]
    hours = (ts_arr - t0) / 3.6e6

    states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
    swaps = detect_swaps(states) if states.height > 0 else pl.DataFrame()

    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(10, 5), height_ratios=[3, 1], sharex=True)
    fig.subplots_adjust(hspace=0.08)

    ax1.fill_between(hours, bid_arr, ask_arr, color=color, alpha=0.2, label="Bid-Ask band")
    ax1.plot(hours, mid_arr, color=color, linewidth=0.6, label=f"{label} mid (USD 100)")

    if swaps.height > 0 and "direction" in swaps.columns:
        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        swap_hours = (swap_ts - t0) / 3.6e6
        bd = swaps["bd"].to_numpy().astype(np.float64)
        qd = swaps["qd"].to_numpy().astype(np.float64)
        exec_price = np.abs(qd / bd) * 1e3

        buy_mask = bd > 0
        sell_mask = bd < 0

        if buy_mask.any():
            ax1.scatter(
                swap_hours[buy_mask],
                exec_price[buy_mask],
                marker="^",
                color="green",
                s=8,
                alpha=0.4,
                zorder=4,
                label=f"Buy SOL ({buy_mask.sum():,})",
            )
        if sell_mask.any():
            ax1.scatter(
                swap_hours[sell_mask],
                exec_price[sell_mask],
                marker="v",
                color="red",
                s=8,
                alpha=0.4,
                zorder=4,
                label=f"Sell SOL ({sell_mask.sum():,})",
            )

    y_lo = min(bid_arr.min(), mid_arr.min()) - 0.05
    y_hi = max(ask_arr.max(), mid_arr.max()) + 0.05
    ax1.set_ylim(y_lo, y_hi)
    ax1.set_ylabel("Price (USD)")
    ax1.set_title(f"{label}: Pool Mid-Price and Swap Executions (USD 100 tier)")
    ax1.legend(loc="upper right", fontsize=7)
    ax1.grid(True, alpha=0.3)

    if swaps.height > 0:
        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        swap_hours = (swap_ts - t0) / 3.6e6
        bd = swaps["bd"].to_numpy().astype(np.float64) / 1e9
        buy_mask = bd > 0
        sell_mask = bd < 0
        bar_w = 0.01
        if buy_mask.any():
            ax2.bar(swap_hours[buy_mask], bd[buy_mask], width=bar_w, color="green", alpha=0.5)
        if sell_mask.any():
            ax2.bar(swap_hours[sell_mask], bd[sell_mask], width=bar_w, color="red", alpha=0.5)
        ax2.axhline(0, color="black", linewidth=0.5)

    ax2.set_xlabel("Time (hours)")
    ax2.set_ylabel("Swap size (SOL)")
    ax2.grid(True, alpha=0.3)

    fig.tight_layout()
    path = os.path.join(out_dir, "plot_price_tracking.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _price_comparison(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.2: Price Comparison\n{SEP}")

    pools = metadata["pools"]
    targets = ["humidifi", "goonfi", "zerofi"]
    target_labels = {"humidifi": "Humidifi", "goonfi": "GoonFi V2", "zerofi": "ZeroFi"}

    plot_data = []
    for amm in targets:
        candidates = [p for p in pools if p["amm_type"] == amm]
        best_pool = None
        best_n = 0
        for c in candidates:
            quotes = load_pool_data(session_dir, c["data_dir"], "quotes")
            n = quotes.filter(pl.col("slot") > 0).height if quotes.height > 0 else 0
            if n > best_n:
                best_pool = c
                best_n = n
        if best_pool is None:
            continue

        quotes = load_pool_data(session_dir, best_pool["data_dir"], "quotes")
        paired = compute_bid_ask(quotes, 100.0)
        if paired.height < 100:
            continue

        states = load_pool_data(session_dir, best_pool["data_dir"], "derived_state")
        swaps = detect_swaps(states) if states.height > 0 else pl.DataFrame()

        plot_data.append((amm, best_pool, paired, swaps))

    if len(plot_data) < 2:
        print("  Not enough protocols with data")
        return

    window_ms = 15 * 60 * 1000
    all_paired_ts = [d[2]["timestamp_ms"].to_numpy().astype(np.float64) for d in plot_data]
    global_start = max(ts[0] for ts in all_paired_ts)
    global_end = min(ts[-1] for ts in all_paired_ts)

    def window_ok(t_lo, t_hi, all_ts, max_gap_s=30):
        for ts_arr in all_ts:
            in_win = ts_arr[(ts_arr >= t_lo) & (ts_arr <= t_hi)]
            if len(in_win) < 10:
                return False
            if (in_win[0] - t_lo) / 1000 > max_gap_s:
                return False
            if (t_hi - in_win[-1]) / 1000 > max_gap_s:
                return False
            if np.max(np.diff(in_win)) / 1000 > max_gap_s:
                return False
        return True

    t_mid = (global_start + global_end) / 2
    step = 60 * 1000
    for offset in range(300):
        for sign in [0, -1, 1]:
            candidate = t_mid + sign * offset * step
            t_lo = candidate - window_ms / 2
            t_hi = candidate + window_ms / 2
            if t_lo >= global_start and t_hi <= global_end:
                if window_ok(t_lo, t_hi, all_paired_ts):
                    t_mid = candidate
                    t_lo = t_mid - window_ms / 2
                    t_hi = t_mid + window_ms / 2
                    break
        else:
            continue
        break

    fig, axes = plt.subplots(len(plot_data), 1, figsize=(10, 2.5 * len(plot_data)), sharex=True)
    if len(plot_data) == 1:
        axes = [axes]

    for idx, (amm, pool, paired, swaps) in enumerate(plot_data):
        ax = axes[idx]
        color = PROTOCOL_COLORS.get(amm, "blue")
        label = target_labels.get(amm, PROTOCOL_LABELS.get(amm, amm))

        ts = paired["timestamp_ms"].to_numpy().astype(np.float64)
        bid = paired["bid"].to_numpy()
        ask = paired["ask"].to_numpy()
        mid = paired["mid"].to_numpy()

        mask = (ts >= t_lo) & (ts <= t_hi)
        ts_w = ts[mask]
        bid_w = bid[mask]
        ask_w = ask[mask]
        mid_w = mid[mask]

        if len(ts_w) < 10:
            ax.set_title(f"{label} (no data in window)")
            continue

        step = max(1, len(ts_w) // 2000)
        sl = slice(None, None, step)

        minutes = (ts_w[sl] - t_lo) / 60000.0

        ax.fill_between(minutes, bid_w[sl], ask_w[sl], color=color, alpha=0.3, label="Bid-Ask")
        ax.plot(minutes, mid_w[sl], color=color, linewidth=1.0, zorder=5, label="Mid")

        if swaps.height > 0 and "direction" in swaps.columns:
            swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
            swap_mask = (swap_ts >= t_lo) & (swap_ts <= t_hi)
            if swap_mask.any():
                s_ts = swap_ts[swap_mask]
                s_min = (s_ts - t_lo) / 60000.0
                bd = swaps["bd"].to_numpy().astype(np.float64)
                qd = swaps["qd"].to_numpy().astype(np.float64)
                bd_w = bd[swap_mask]
                qd_w = qd[swap_mask]
                exec_p = np.abs(qd_w / bd_w) * 1e3

                buy = bd_w > 0
                sell = bd_w < 0
                p_lo = bid_w.min() - 0.05
                p_hi = ask_w.max() + 0.05
                valid = (exec_p >= p_lo) & (exec_p <= p_hi)

                if (buy & valid).any():
                    ax.scatter(
                        s_min[buy & valid],
                        exec_p[buy & valid],
                        marker="^",
                        color="green",
                        s=6,
                        alpha=0.3,
                        zorder=3,
                        label=f"Buy ({buy.sum()})",
                    )
                if (sell & valid).any():
                    ax.scatter(
                        s_min[sell & valid],
                        exec_p[sell & valid],
                        marker="v",
                        color="red",
                        s=6,
                        alpha=0.3,
                        zorder=3,
                        label=f"Sell ({sell.sum()})",
                    )

        p1 = np.percentile(mid_w, 1)
        p99 = np.percentile(mid_w, 99)
        margin = (p99 - p1) * 0.3
        y_lo = p1 - margin
        y_hi = p99 + margin
        ax.set_ylim(y_lo, y_hi)

        ax.set_ylabel("Price (USD)")
        ax.set_title(label, fontsize=10)
        ax.legend(loc="upper right", fontsize=6, ncol=2)
        ax.grid(True, alpha=0.3)

    axes[-1].set_xlabel("Time (minutes)")
    fig.suptitle(
        "Quoting Behaviour Comparison (15-minute window, USD 100 tier)", fontsize=11, y=1.01
    )
    fig.tight_layout()
    path_out = os.path.join(out_dir, "plot_price_comparison.pdf")
    fig.savefig(path_out, bbox_inches="tight")
    plt.close(fig)
    print(f"Saved: {path_out}")


def _spread_stats_table(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.2: Spread Stats Table\n{SEP}")

    pools = metadata["pools"]
    proto_spreads = defaultdict(list)

    for pool in pools:
        amm = pool["amm_type"]
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        if quotes.height == 0:
            continue
        paired = compute_bid_ask(quotes, 100.0)
        if paired.height == 0:
            continue
        valid = paired.filter(pl.col("spread_bps") >= 0)
        if valid.height > 0:
            proto_spreads[amm].extend(valid["spread_bps"].to_list())

    rows = []
    for amm in sorted(proto_spreads.keys()):
        vals = [v for v in proto_spreads[amm] if np.isfinite(v)]
        if vals:
            arr = np.array(vals)
            rows.append({
                "protocol": PROTOCOL_LABELS.get(amm, amm),
                "mean_spread_bps": round(np.mean(arr), 2),
                "median_spread_bps": round(np.median(arr), 2),
                "std_spread_bps": round(np.std(arr), 2),
                "p5_spread_bps": round(np.percentile(arr, 5), 2),
                "p95_spread_bps": round(np.percentile(arr, 95), 2),
            })

    if rows:
        df_out = pl.DataFrame(rows)
        path = os.path.join(out_dir, "table_spread_stats.csv")
        df_out.write_csv(path)
        print(df_out)
        print(f"Saved: {path}")


def section_risk(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    _inventory_imbalance(session_dir, metadata, pyth_sol, out_dir)
    _spread_vs_imbalance(session_dir, metadata, pyth_sol, out_dir)
    _pnl_table(session_dir, metadata, pyth_sol, out_dir)
    _pnl_over_time(session_dir, metadata, pyth_sol, out_dir)
    _jit_patterns(session_dir, metadata, out_dir)


def _pnl_over_time(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.3: PnL Over Time\n{SEP}")
    import numpy as np

    fig, ax = plt.subplots(figsize=(8, 4))

    protocol_series = {}
    protocol_series_ch = {}
    protocol_series_hum = {}

    _hum_oracle_ts = None
    _hum_oracle_px = None
    try:
        _q_files = sorted(
            glob.glob(os.path.join(session_dir, "quoter_output", "SOL-USDC", "quotes_*.parquet"))
        )
        if _q_files:
            _quotes = pl.concat([pl.read_parquet(f) for f in _q_files])
            _quotes = _quotes.filter(pl.col("slot") > 0).filter(pl.col("output_amount") > 0)
            _PS = 1e3

            _b2q = (
                _quotes
                .filter(
                    (pl.col("direction") == "B2Q")
                    & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1)
                )
                .with_columns(
                    (
                        pl.col("output_amount").cast(pl.Float64)
                        / pl.col("input_amount").cast(pl.Float64)
                        * _PS
                    ).alias("bid")
                )
                .select(["timestamp_ms", "slot", "pool_id", "bid"])
            )
            _q2b = (
                _quotes
                .filter(
                    (pl.col("direction") == "Q2B")
                    & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1)
                )
                .with_columns(
                    (
                        pl.col("input_amount").cast(pl.Float64)
                        / pl.col("output_amount").cast(pl.Float64)
                        * _PS
                    ).alias("ask")
                )
                .select(["slot", "pool_id", "ask"])
            )
            _mids = (
                _b2q
                .join(_q2b, on=["slot", "pool_id"], how="inner")
                .with_columns(((pl.col("bid") + pl.col("ask")) / 2).alias("mid"))
                .select(["timestamp_ms", "slot", "pool_id", "mid"])
                .sort("timestamp_ms")
            )

            _pool_ids = _mids["pool_id"].unique().sort().to_list()
            _pool_map = {pid: i for i, pid in enumerate(_pool_ids)}
            _n_pools = len(_pool_ids)

            _ts_arr = _mids["timestamp_ms"].to_numpy().astype(np.float64)
            _mid_arr = _mids["mid"].to_numpy()
            _pidx_arr = np.array([_pool_map[p] for p in _mids["pool_id"].to_list()])

            _latest = np.full(_n_pools, np.nan)
            _con_ts = np.empty(len(_ts_arr))
            _con_px = np.empty(len(_ts_arr))
            _valid_count = 0

            for i in range(len(_ts_arr)):
                _latest[_pidx_arr[i]] = _mid_arr[i]
                _valid = _latest[~np.isnan(_latest)]
                if len(_valid) >= 3:
                    _con_ts[_valid_count] = _ts_arr[i]
                    _con_px[_valid_count] = np.median(_valid)
                    _valid_count += 1

            _hum_oracle_ts = _con_ts[:_valid_count].copy()
            _hum_oracle_px = _con_px[:_valid_count].copy()
            print(f"  pAMM consensus oracle: {_valid_count} prices from {_n_pools} pools")
    except Exception as e:
        print(f"  pAMM oracle build failed: {e}")

    def _hum_vec(ts_arr):
        if _hum_oracle_ts is None:
            return pyth_sol.vec(ts_arr)
        idxs = np.searchsorted(_hum_oracle_ts, ts_arr, side="right") - 1
        return _hum_oracle_px[np.clip(idxs, 0, len(_hum_oracle_px) - 1)]

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        bcol, qcol = "base_vault_balance", "quote_vault_balance"
        if bcol not in states.columns or qcol not in states.columns:
            continue

        clean = (
            states
            .filter(pl.col("slot") > 0)
            .filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT))
            .filter((pl.col(qcol) > 0) & (pl.col(qcol) < MAX_VAULT))
            .sort(["slot", "write_version"])
        )

        if clean.height <= 50:
            continue
        clean = clean.slice(50)

        ts = clean["timestamp_ms"].to_numpy()
        base = clean[bcol].to_numpy().astype(np.float64)
        quote = clean[qcol].to_numpy().astype(np.float64)
        prices = pyth_sol.vec(ts)

        base_sol = base / 1e9
        quote_usd = quote / 1e6
        seg_ids, is_capital = filter_capital_events(base_sol, quote_usd)
        n_capital = is_capital.sum()
        if n_capital > 0:
            pool_label = pool.get("pool_id", "")[:8]
            print(
                f"  {pool_label}: {n_capital} capital events filtered, {seg_ids[-1] + 1} segments"
            )

        cum_pnl = np.zeros(len(base))
        running_pnl = 0.0
        for seg in range(seg_ids[-1] + 1):
            mask = seg_ids == seg
            idx = np.where(mask)[0]
            if len(idx) < 2:
                cum_pnl[idx] = running_pnl
                continue
            seg_base = base_sol[idx]
            seg_quote = quote_usd[idx]
            seg_prices = prices[idx]
            seg_pnl = (seg_base - seg_base[0]) * seg_prices + (seg_quote - seg_quote[0])
            cum_pnl[idx] = running_pnl + seg_pnl
            running_pnl += seg_pnl[-1]

        if amm not in protocol_series:
            protocol_series[amm] = []
        protocol_series[amm].append((ts, cum_pnl))

        ch_pnl = np.zeros(len(base))
        ch_running = 0.0
        for seg in range(seg_ids[-1] + 1):
            mask = seg_ids == seg
            idx = np.where(mask)[0]
            if len(idx) < 2:
                ch_pnl[idx] = ch_running
                continue
            seg_b = base_sol[idx]
            seg_q = quote_usd[idx]
            seg_p = prices[idx]
            d_b = np.diff(seg_b)
            d_q = np.diff(seg_q)
            inc = d_q + d_b * seg_p[1:]
            seg_cum = np.concatenate([[0.0], np.cumsum(inc)])
            ch_pnl[idx] = ch_running + seg_cum
            ch_running += seg_cum[-1]

        if amm not in protocol_series_ch:
            protocol_series_ch[amm] = []
        protocol_series_ch[amm].append((ts, ch_pnl))

        hum_pnl = np.zeros(len(base))
        hum_running = 0.0
        hum_prices = _hum_vec(ts)
        for seg in range(seg_ids[-1] + 1):
            mask = seg_ids == seg
            idx = np.where(mask)[0]
            if len(idx) < 2:
                hum_pnl[idx] = hum_running
                continue
            seg_b = base_sol[idx]
            seg_q = quote_usd[idx]
            seg_hp = hum_prices[idx]
            d_b = np.diff(seg_b)
            d_q = np.diff(seg_q)
            inc = d_q + d_b * seg_hp[1:]
            seg_cum = np.concatenate([[0.0], np.cumsum(inc)])
            hum_pnl[idx] = hum_running + seg_cum
            hum_running += seg_cum[-1]

        if amm not in protocol_series_hum:
            protocol_series_hum[amm] = []
        protocol_series_hum[amm].append((ts, hum_pnl))

    if not protocol_series:
        print("  No PnL data available")
        plt.close(fig)
        return

    if not protocol_series_ch:
        protocol_series_ch = {}

    all_t0 = []
    for series_list in protocol_series.values():
        for ts_arr, _ in series_list:
            all_t0.append(ts_arr[0])
    global_t0 = min(all_t0)

    for amm in sorted(protocol_series.keys()):
        series_list = protocol_series[amm]
        if len(series_list) == 1:
            ts, pnl = series_list[0]
            time_h = (ts - global_t0) / 3.6e6
        else:
            all_ts = np.sort(np.unique(np.concatenate([s[0] for s in series_list])))
            summed_pnl = np.zeros(len(all_ts))
            for ts, pnl in series_list:
                interped = np.interp(all_ts, ts, pnl)
                summed_pnl += interped
            ts = all_ts
            pnl = summed_pnl
            time_h = (ts - global_t0) / 3.6e6

        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)
        ax.plot(time_h, pnl, linewidth=1.5, color=color, label=label)

    ax.axhline(0, color="black", linewidth=0.5, linestyle="--")
    ax.set_xlabel("Time (hours)")
    ax.set_ylabel("Cumulative PnL (USD)")
    ax.set_title("LP vs HODL PnL Over Time (SOL/USDC)")
    ax.legend(fontsize=7, loc="best")
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    path_out = os.path.join(out_dir, "plot_pnl_over_time.pdf")
    fig.savefig(path_out)
    plt.close(fig)
    print(f"Saved: {path_out}")

    fig2, ax2 = plt.subplots(figsize=(8, 4))

    for amm in sorted(protocol_series_ch.keys()):
        series_list = protocol_series_ch[amm]
        if len(series_list) == 1:
            ts, pnl = series_list[0]
            time_h = (ts - global_t0) / 3.6e6
        else:
            all_ts = np.sort(np.unique(np.concatenate([s[0] for s in series_list])))
            summed_pnl = np.zeros(len(all_ts))
            for ts, pnl in series_list:
                interped = np.interp(all_ts, ts, pnl)
                summed_pnl += interped
            ts = all_ts
            pnl = summed_pnl
            time_h = (ts - global_t0) / 3.6e6

        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)
        ax2.plot(time_h, pnl, linewidth=1.5, color=color, label=label)

    ax2.axhline(0, color="black", linewidth=0.5, linestyle="--")
    ax2.set_xlabel("Time (hours)")
    ax2.set_ylabel("Cumulative PnL (USD)")
    ax2.set_title("Continuous Hedging PnL Over Time — Pyth Oracle (SOL/USDC)")
    ax2.legend(fontsize=7, loc="best")
    ax2.grid(True, alpha=0.3)
    fig2.tight_layout()
    path_ch = os.path.join(out_dir, "plot_pnl_continuous_hedging.pdf")
    fig2.savefig(path_ch)
    plt.close(fig2)
    print(f"Saved: {path_ch}")

    fig3, ax3 = plt.subplots(figsize=(8, 4))

    for amm in sorted(protocol_series_hum.keys()):
        series_list = protocol_series_hum[amm]
        if len(series_list) == 1:
            ts, pnl = series_list[0]
            time_h = (ts - global_t0) / 3.6e6
        else:
            all_ts = np.sort(np.unique(np.concatenate([s[0] for s in series_list])))
            summed_pnl = np.zeros(len(all_ts))
            for ts, pnl in series_list:
                interped = np.interp(all_ts, ts, pnl)
                summed_pnl += interped
            ts = all_ts
            pnl = summed_pnl
            time_h = (ts - global_t0) / 3.6e6

        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)
        ax3.plot(time_h, pnl, linewidth=1.5, color=color, label=label)

    ax3.axhline(0, color="black", linewidth=0.5, linestyle="--")
    ax3.set_xlabel("Time (hours)")
    ax3.set_ylabel("Cumulative PnL (USD)")
    ax3.set_title("Continuous Hedging PnL Over Time — pAMM Consensus Oracle (SOL/USDC)")
    ax3.legend(fontsize=7, loc="best")
    ax3.grid(True, alpha=0.3)
    fig3.tight_layout()
    path_hum = os.path.join(out_dir, "plot_pnl_pamm_oracle.pdf")
    fig3.savefig(path_hum)
    plt.close(fig3)
    print(f"Saved: {path_hum}")


def _inventory_imbalance(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.3: Inventory Imbalance\n{SEP}")

    pools = metadata["pools"]
    target_amms = ["humidifi", "solfiv2", "goonfi", "zerofi"]

    fig, axes = plt.subplots(2, 2, figsize=(10, 6))
    fig.suptitle(
        r"Inventory Imbalance $q(t) = (v_{\mathrm{base}}^{\$} - v_{\mathrm{quote}}^{\$}) \;/\; \mathrm{TVL}$",
        fontsize=11,
    )

    for idx, amm in enumerate(target_amms):
        ax = axes.flat[idx]
        candidates = [p for p in pools if p["amm_type"] == amm]
        pool = None
        for candidate in sorted(
            candidates,
            key=lambda p: load_pool_data(session_dir, p["data_dir"], "derived_state").height,
            reverse=True,
        ):
            states_check = load_pool_data(session_dir, candidate["data_dir"], "derived_state")
            if states_check.height < 100:
                continue
            bcol_c, qcol_c = "base_vault_balance", "quote_vault_balance"
            if bcol_c not in states_check.columns:
                continue
            b = states_check.filter(pl.col("slot") > 0)[bcol_c].to_numpy().astype(np.float64) / 1e9
            db = np.abs(np.diff(b))
            q = states_check.filter(pl.col("slot") > 0)[qcol_c].to_numpy().astype(np.float64) / 1e6
            dq = np.abs(np.diff(q))
            has_withdrawal = ((db > 100) & (dq < 1.0)).any()
            if not has_withdrawal:
                pool = candidate
                break
        if pool is None:
            pool = find_primary_pool(pools, amm, session_dir)
        if pool is None:
            ax.set_title(PROTOCOL_LABELS.get(amm, amm) + " (no data)")
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        bcol, qcol = "base_vault_balance", "quote_vault_balance"
        if states.height == 0 or bcol not in states.columns:
            ax.set_title(PROTOCOL_LABELS.get(amm, amm) + " (no data)")
            continue

        real = (
            states
            .filter(pl.col("slot") > 0)
            .filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT))
            .sort("timestamp_ms")
        )

        if real.height > 100:
            real = real.slice(50)

        step = max(1, real.height // 5000)
        sub = real.gather_every(step)

        ts = sub["timestamp_ms"].to_numpy()
        t0 = ts[0]
        hours = (ts - t0) / 3.6e6

        base_vals = sub[bcol].to_numpy().astype(np.float64)
        quote_vals = sub[qcol].to_numpy().astype(np.float64)
        sol_prices = pyth_sol.vec(ts)

        base_usd = base_vals / 1e9 * sol_prices
        quote_usd = quote_vals / 1e6
        tvl = base_usd + quote_usd

        mask = tvl > 0
        q = np.full_like(tvl, np.nan)
        q[mask] = (base_usd[mask] - quote_usd[mask]) / tvl[mask]

        color = PROTOCOL_COLORS.get(amm, "blue")
        ax.plot(hours[mask], q[mask], linewidth=0.3, color=color)
        mean_q = np.nanmean(q[mask])
        ax.axhline(y=mean_q, color="red", linestyle="--", linewidth=0.8, label=f"mean={mean_q:.3f}")
        ax.axhline(y=0, color="black", linewidth=0.3, linestyle=":")
        ax.set_title(PROTOCOL_LABELS.get(amm, amm))
        ax.set_ylabel("$q(t)$")
        ax.legend(fontsize=7, loc="upper right")

    axes[1, 0].set_xlabel("Time (hours)")
    axes[1, 1].set_xlabel("Time (hours)")
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_inventory_imbalance.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _spread_vs_imbalance(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.3: Spread vs Imbalance (ZeroFi)\n{SEP}")

    pool = find_primary_pool(metadata["pools"], "zerofi", session_dir)
    if pool is None:
        print("  ZeroFi pool not found, skipping")
        return

    states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
    bcol, qcol = "base_vault_balance", "quote_vault_balance"
    if states.height == 0 or bcol not in states.columns:
        print("  No state data, skipping")
        return

    st = states.filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT)).sort("timestamp_ms")

    ts = st["timestamp_ms"].to_numpy()
    base_usd = st[bcol].to_numpy().astype(np.float64) / 1e9 * pyth_sol.vec(ts)
    quote_usd = st[qcol].to_numpy().astype(np.float64) / 1e6
    tvl = base_usd + quote_usd
    mask = tvl > 0
    abs_q = np.abs((base_usd - quote_usd) / tvl)
    abs_q[~mask] = np.nan
    st_ts = ts
    st_abs_q = abs_q

    quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
    if quotes.height == 0:
        print("  No quote data, skipping")
        return

    paired = compute_bid_ask(quotes, 100.0)
    if paired.height < 10:
        print("  Insufficient spread data, skipping")
        return

    paired = paired.filter(pl.col("spread_bps") >= 0)
    paired_ts = paired["timestamp_ms"].to_numpy()
    spread_bps = paired["spread_bps"].to_numpy()

    idxs = np.searchsorted(st_ts, paired_ts, side="right") - 1
    idxs = np.clip(idxs, 0, len(st_ts) - 1)
    q_at_quote = st_abs_q[idxs]

    valid = np.isfinite(q_at_quote) & np.isfinite(spread_bps)
    q_vals = q_at_quote[valid]
    s_vals = spread_bps[valid]

    if len(q_vals) < 20:
        print("  Too few data points, skipping")
        return

    rho, pval = scipy_stats.spearmanr(q_vals, s_vals)

    fig, ax = plt.subplots(figsize=(5, 4))
    ax.scatter(
        q_vals,
        s_vals,
        alpha=0.05,
        s=2,
        color=PROTOCOL_COLORS.get("zerofi", PROTOCOL_COLORS.get("zero_fi", "red")),
    )

    n_bins = 20
    bin_edges = np.linspace(q_vals.min(), q_vals.max(), n_bins + 1)
    bin_centers = []
    bin_means = []
    for i in range(n_bins):
        in_bin = (q_vals >= bin_edges[i]) & (q_vals < bin_edges[i + 1])
        if in_bin.sum() > 5:
            bin_centers.append((bin_edges[i] + bin_edges[i + 1]) / 2)
            bin_means.append(np.mean(s_vals[in_bin]))
    if bin_centers:
        ax.plot(
            bin_centers,
            bin_means,
            color="black",
            linewidth=2,
            marker="o",
            markersize=4,
            label="Binned mean",
        )

    p95 = np.percentile(s_vals, 95)
    ax.set_ylim(0, p95 * 1.5)
    ax.set_xlabel("|q| (absolute imbalance)")
    ax.set_ylabel("Spread (bps) at USD 100")
    ax.set_title("ZeroFi: Spread vs Inventory Imbalance")
    ax.annotate(
        f"Spearman rho = {rho:.2f} (p={pval:.2e})",
        xy=(0.05, 0.95),
        xycoords="axes fraction",
        fontsize=8,
        verticalalignment="top",
        bbox=dict(boxstyle="round,pad=0.3", facecolor="wheat", alpha=0.5),
    )
    ax.legend(loc="lower right", fontsize=8)
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_spread_vs_imbalance.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _pnl_table(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.3: PnL Table\n{SEP}")

    rows = []
    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        bcol, qcol = "base_vault_balance", "quote_vault_balance"
        if bcol not in states.columns or qcol not in states.columns:
            continue

        clean = (
            states
            .filter(pl.col("slot") > 0)
            .filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT))
            .filter((pl.col(qcol) > 0) & (pl.col(qcol) < MAX_VAULT))
            .sort(["slot", "write_version"])
        )

        if clean.height <= 50:
            continue
        clean = clean.slice(50)

        swaps = detect_swaps(states)
        n_swaps = swaps.height if swaps.height > 0 else 0
        vol_usd = swaps["qd"].abs().sum() / 1e6 if n_swaps > 0 else 0.0

        ts = clean["timestamp_ms"].to_numpy()
        base_arr = clean[bcol].to_numpy().astype(np.float64) / 1e9
        quote_arr = clean[qcol].to_numpy().astype(np.float64) / 1e6
        prices_arr = pyth_sol.vec(ts)

        seg_ids, is_capital = filter_capital_events(base_arr, quote_arr)

        dn_pnl = 0.0
        for seg in range(seg_ids[-1] + 1):
            idx = np.where(seg_ids == seg)[0]
            if len(idx) < 2:
                continue
            seg_pnl = (base_arr[idx[-1]] - base_arr[idx[0]]) * prices_arr[idx[-1]] + (
                quote_arr[idx[-1]] - quote_arr[idx[0]]
            )
            dn_pnl += seg_pnl

        first = clean.head(1)
        last = clean.tail(1)
        ts_last = last["timestamp_ms"][0]

        last = clean.tail(1)
        ts_last = last["timestamp_ms"][0]
        bv, qv = last[bcol][0], last[qcol][0]
        sol_end = pyth_sol.at(ts_last)
        tvl = bv / 1e9 * sol_end + qv / 1e6

        ts_first = first["timestamp_ms"][0]
        dur_h = (ts_last - ts_first) / 3.6e6
        ann_pct = (dn_pnl / tvl) * (8760.0 / dur_h) * 100.0 if tvl > 0 and dur_h > 0 else 0.0

        ch_pnl = 0.0
        for seg in range(seg_ids[-1] + 1):
            idx = np.where(seg_ids == seg)[0]
            if len(idx) < 2:
                continue
            d_b = np.diff(base_arr[idx])
            d_q = np.diff(quote_arr[idx])
            ch_pnl += (d_q + d_b * prices_arr[idx[1:]]).sum()

        ch_ann_pct = (ch_pnl / tvl) * (8760.0 / dur_h) * 100.0 if tvl > 0 and dur_h > 0 else 0.0

        rows.append({
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "pool_id": pool["pool_id"][:12],
            "swaps": n_swaps,
            "volume_usd": round(vol_usd, 2),
            "tvl_usd": round(tvl, 2),
            "dn_pnl_usd": round(dn_pnl, 4),
            "annualized_pct": round(ann_pct, 2),
            "ch_pnl_usd": round(ch_pnl, 4),
            "ch_ann_pct": round(ch_ann_pct, 2),
        })

    if rows:
        df_out = pl.DataFrame(rows)
        path = os.path.join(out_dir, "table_pnl.csv")
        df_out.write_csv(path)
        print(df_out)
        print(f"Saved: {path}")


def _jit_patterns(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.3: JIT Patterns\n{SEP}")

    jit_data = {}
    bcol, qcol = "base_vault_balance", "quote_vault_balance"

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS:
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0 or bcol not in states.columns:
            continue

        real = (
            states
            .filter(pl.col("slot") > 0)
            .filter(pl.col("txn_signature").is_not_null() & (pl.col("txn_signature") != ""))
            .filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT))
            .sort(["slot", "write_version"])
        )

        swaps = detect_swaps(states)
        if swaps.height == 0:
            continue

        swap_sigs = set(swaps["txn_signature"].to_list())

        sig_counts = (
            real
            .filter(pl.col("txn_signature").is_in(list(swap_sigs)))
            .group_by("txn_signature")
            .len()
        )

        n_jit = sig_counts.filter(pl.col("len") > 3).height
        n_total = len(swap_sigs)

        if amm not in jit_data:
            jit_data[amm] = {"total": 0, "jit": 0}
        jit_data[amm]["total"] += n_total
        jit_data[amm]["jit"] += n_jit

    if not jit_data:
        print("  No swap data for JIT analysis")
        return

    for amm, d in jit_data.items():
        d["pct"] = 100 * d["jit"] / d["total"] if d["total"] > 0 else 0
        print(
            f"  {PROTOCOL_LABELS.get(amm, amm)}: {d['total']} swaps, {d['jit']} JIT ({d['pct']:.1f}%)"
        )

    jit_data = {k: v for k, v in jit_data.items() if k not in EXCLUDE_CROSS_COMPARE}

    fig, ax = plt.subplots(figsize=(6, 3.5))
    amms_sorted = sorted(jit_data.keys(), key=lambda k: jit_data[k]["pct"])
    x = range(len(amms_sorted))
    pcts = [jit_data[k]["pct"] for k in amms_sorted]
    colors = [PROTOCOL_COLORS.get(k, "gray") for k in amms_sorted]
    bars = ax.bar(x, pcts, color=colors, edgecolor="black", linewidth=0.3)

    ymax = max(pcts) * 1.3 if pcts and max(pcts) > 0 else 105
    for i, k in enumerate(amms_sorted):
        label_y = pcts[i] + ymax * 0.03
        ax.text(i, label_y, f"n={jit_data[k]['total']}", ha="center", va="bottom", fontsize=7)

    ax.set_xticks(list(x))
    ax.set_xticklabels([PROTOCOL_LABELS.get(k, k) for k in amms_sorted], rotation=30, ha="right")
    ax.set_ylabel("Same-tx JIT (%)")
    ax.set_title("Same-Transaction JIT Update Percentage")
    ax.set_ylim(0, ymax)
    ax.grid(True, alpha=0.3, axis="y")
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_jit_patterns.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def section_competition(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    _market_share(session_dir, metadata, out_dir)
    _swap_size_cdf(session_dir, metadata, out_dir)
    _swap_size_share(session_dir, metadata, out_dir)
    _temporal_activity(session_dir, metadata, out_dir)
    _cross_protocol_flow(session_dir, metadata, out_dir)
    _routing_matrix(session_dir, metadata, out_dir)


def _market_share(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.4: Market Share\n{SEP}")

    proto_volume = defaultdict(float)
    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS:
            continue
        swaps = detect_swaps_for_pool(session_dir, pool)
        if swaps.height > 0:
            proto_volume[amm] += swaps["qd"].abs().sum() / 1e6

    if not proto_volume:
        print("  No volume data, skipping")
        return

    total_vol = sum(proto_volume.values())
    fig, ax = plt.subplots(figsize=(6, 4))
    amms = sorted(proto_volume.keys(), key=lambda a: proto_volume[a], reverse=True)
    labels = [PROTOCOL_LABELS.get(a, a) for a in amms]
    pcts = [100 * proto_volume[a] / total_vol for a in amms]
    colors = [PROTOCOL_COLORS.get(a, "gray") for a in amms]

    bars = ax.bar(labels, pcts, color=colors, edgecolor="black", linewidth=0.5)
    for bar, a, pct in zip(bars, amms, pcts):
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            bar.get_height() + 0.5,
            f"USD {proto_volume[a]:.0f}",
            ha="center",
            va="bottom",
            fontsize=7,
        )

    ax.set_ylabel("Volume share (%)")
    ax.set_title("Volume Share by Protocol")
    ax.grid(True, alpha=0.3, axis="y")
    plt.xticks(rotation=30, ha="right")
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_market_share.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _swap_size_cdf(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.4: Swap Size Distribution\n{SEP}")

    proto_sizes = defaultdict(list)
    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS:
            continue
        swaps = detect_swaps_for_pool(session_dir, pool)
        if swaps.height > 0:
            sizes = (swaps["qd"].abs() / 1e6).to_numpy()
            proto_sizes[amm].extend(sizes[sizes > 0].tolist())

    fig, ax = plt.subplots(figsize=(6, 4))
    for amm in sorted(proto_sizes.keys()):
        arr = np.sort(proto_sizes[amm])
        if len(arr) == 0:
            continue
        cdf = np.arange(1, len(arr) + 1) / len(arr)
        ax.plot(
            arr,
            cdf,
            color=PROTOCOL_COLORS.get(amm, "black"),
            label=PROTOCOL_LABELS.get(amm, amm),
            linewidth=1.5,
        )

    ax.set_xscale("log")
    ax.set_xlabel("Swap size (USD)")
    ax.set_ylabel("CDF")
    ax.set_title("Cumulative Distribution of Swap Sizes")
    ax.legend(loc="lower right", fontsize=8)
    ax.grid(True, alpha=0.3, which="both")
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_swap_size_cdf.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _swap_size_share(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.4: Swap Size Share by Bin\n{SEP}")

    BIN_EDGES = [0, 10, 100, 1_000, 10_000, float("inf")]
    BIN_LABELS = ["$0\u201310", "$10\u2013100", "$100\u20131K", "$1K\u201310K", "$10K+"]

    proto_sizes = defaultdict(list)
    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        swaps = detect_swaps_for_pool(session_dir, pool)
        if swaps.height > 0:
            sizes = (swaps["qd"].abs() / 1e6).to_numpy()
            proto_sizes[amm].extend(sizes[sizes > 0].tolist())

    if not proto_sizes:
        print("  No swap data, skipping")
        return

    proto_bin_counts = {}
    for amm, sizes in proto_sizes.items():
        arr = np.array(sizes)
        counts = []
        for lo, hi in zip(BIN_EDGES[:-1], BIN_EDGES[1:]):
            counts.append(int(np.sum((arr >= lo) & (arr < hi))))
        proto_bin_counts[amm] = counts

    n_bins = len(BIN_LABELS)
    bin_totals = np.zeros(n_bins)
    for counts in proto_bin_counts.values():
        bin_totals += np.array(counts)

    proto_bin_pct = {}
    for amm, counts in proto_bin_counts.items():
        pct = []
        for i, c in enumerate(counts):
            pct.append(100.0 * c / bin_totals[i] if bin_totals[i] > 0 else 0.0)
        proto_bin_pct[amm] = pct

    amms_sorted = sorted(proto_bin_pct.keys(), key=lambda a: sum(proto_bin_counts[a]), reverse=True)

    fig, ax = plt.subplots(figsize=(6, 4))
    x = np.arange(n_bins)
    bar_width = 0.6
    bottom = np.zeros(n_bins)

    for amm in amms_sorted:
        pct = np.array(proto_bin_pct[amm])
        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)
        ax.bar(
            x,
            pct,
            bar_width,
            bottom=bottom,
            color=color,
            edgecolor="white",
            linewidth=0.3,
            label=label,
        )
        bottom += pct

    ax.set_xticks(x)
    ax.set_xticklabels(BIN_LABELS, fontsize=9)
    ax.set_xlabel("Swap size (USD)")
    ax.set_ylabel("Share of swap count (%)")
    ax.set_title("Protocol Market Share by Swap Size")
    ax.set_ylim(0, 105)
    ax.legend(loc="upper left", fontsize=7, bbox_to_anchor=(1.01, 1.0))
    ax.grid(True, alpha=0.2, axis="y")

    fig.tight_layout()
    path = os.path.join(out_dir, "plot_swap_size_share.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")


def _temporal_activity(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.4: Temporal Activity\n{SEP}")

    proto_buys = defaultdict(list)
    proto_sells = defaultdict(list)
    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        swaps = detect_swaps_for_pool(session_dir, pool)
        if swaps.height > 0 and "direction" in swaps.columns:
            b2q = swaps.filter(pl.col("direction") == "B2Q")
            q2b = swaps.filter(pl.col("direction") == "Q2B")
            proto_buys[amm].extend(b2q["timestamp_ms"].to_list())
            proto_sells[amm].extend(q2b["timestamp_ms"].to_list())

    all_amms = sorted(set(proto_buys.keys()) | set(proto_sells.keys()))
    if not all_amms:
        print("  No swap data, skipping")
        return

    all_ts = []
    for amm in all_amms:
        all_ts.extend(proto_buys.get(amm, []))
        all_ts.extend(proto_sells.get(amm, []))
    t_min, t_max = min(all_ts), max(all_ts)
    bin_ms = 5 * 60 * 1000
    bins = np.arange(t_min, t_max + bin_ms, bin_ms)
    hours = ((bins[:-1] + bins[1:]) / 2 - t_min) / 3.6e6

    fig, axes = plt.subplots(
        len(all_amms), 1, figsize=(8, 2.2 * len(all_amms)), sharex=True, squeeze=False
    )

    for idx, amm in enumerate(all_amms):
        ax = axes[idx, 0]
        buys = proto_buys.get(amm, [])
        sells = proto_sells.get(amm, [])
        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)

        if buys:
            buy_counts, _ = np.histogram(buys, bins=bins)
            ax.bar(
                hours,
                buy_counts,
                width=(bin_ms / 3.6e6) * 0.8,
                color=color,
                alpha=0.7,
                label=f"Buy SOL (n={len(buys)})",
            )
        if sells:
            sell_counts, _ = np.histogram(sells, bins=bins)
            ax.bar(
                hours,
                -sell_counts,
                width=(bin_ms / 3.6e6) * 0.8,
                color=color,
                alpha=0.35,
                label=f"Sell SOL (n={len(sells)})",
            )

        ax.axhline(0, color="black", linewidth=0.3)
        ax.set_ylabel("Swaps / bin")
        ax.set_title(label, fontsize=10)
        ax.legend(fontsize=7, loc="upper right")
        ax.grid(True, alpha=0.2, axis="y")

    axes[-1, 0].set_xlabel("Time (hours)")
    fig.suptitle("Temporal Swap Activity (Buy vs Sell)", fontsize=12, y=1.01)
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_temporal_activity.pdf")
    fig.savefig(path, bbox_inches="tight")
    plt.close(fig)
    print(f"Saved: {path}")


def _cross_protocol_flow(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.4: Cross-Protocol Flow\n{SEP}")

    proto_slots = defaultdict(set)
    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS:
            continue
        swaps = detect_swaps_for_pool(session_dir, pool)
        if swaps.height > 0:
            proto_slots[amm].update(swaps["slot"].unique().to_list())

    amms = sorted(proto_slots.keys())
    n = len(amms)
    if n < 2:
        print("  Need at least 2 protocols, skipping")
        return

    rows = []
    for i, a in enumerate(amms):
        row = {"protocol": PROTOCOL_LABELS.get(a, a)}
        for j, b in enumerate(amms):
            inter = len(proto_slots[a] & proto_slots[b])
            union = len(proto_slots[a] | proto_slots[b])
            row[PROTOCOL_LABELS.get(b, b)] = round(inter / union, 3) if union > 0 else 0
        others = set()
        for b in amms:
            if b != a:
                others.update(proto_slots[b])
        exclusive = proto_slots[a] - others
        row["exclusive_pct"] = (
            round(100 * len(exclusive) / len(proto_slots[a]), 1) if proto_slots[a] else 0
        )
        rows.append(row)

    df_out = pl.DataFrame(rows)
    path = os.path.join(out_dir, "table_cross_protocol_flow.csv")
    df_out.write_csv(path)
    print(df_out)
    print(f"Saved: {path}")


def _routing_matrix(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.4: Routing Efficiency Matrix\n{SEP}")

    route_csv = os.path.join(out_dir, "route_analysis.csv")
    if not os.path.isfile(route_csv):
        print(f"  route_analysis.csv not found in {out_dir}, skipping")
        return

    df = pl.read_csv(route_csv)
    print(f"  Loaded {df.height} candidate rows")

    swap_groups = df.group_by([
        "swap_ts_ms",
        "swap_slot",
        "swap_pool_id",
        "swap_amm_type",
        "actual_output",
    ]).agg([
        pl.col("candidate_pool_id"),
        pl.col("candidate_amm_type"),
        pl.col("candidate_output"),
    ])
    n_swaps = swap_groups.height
    print(f"  {n_swaps} unique swaps")

    PROTO_ORDER = ["humidifi", "bisonfi", "tesserav", "solfiv2", "goonfi", "zerofi"]

    same_pool = defaultdict(int)
    intra_proto = defaultdict(int)
    cross_proto = defaultdict(lambda: defaultdict(int))
    total = defaultdict(int)

    for row in swap_groups.iter_rows(named=True):
        swap_amm = row["swap_amm_type"]
        swap_pool = row["swap_pool_id"]
        actual = row["actual_output"]

        cand_outputs = row["candidate_output"]
        cand_amms = row["candidate_amm_type"]
        cand_pools = row["candidate_pool_id"]

        best_output = actual
        best_amm = swap_amm
        best_pool = swap_pool

        for i in range(len(cand_outputs)):
            if cand_outputs[i] > best_output:
                best_output = cand_outputs[i]
                best_amm = cand_amms[i]
                best_pool = cand_pools[i]

        total[swap_amm] += 1

        if best_pool == swap_pool:
            same_pool[swap_amm] += 1
        elif best_amm == swap_amm:
            intra_proto[swap_amm] += 1
        else:
            cross_proto[swap_amm][best_amm] += 1

    print("\n  Decomposed Routing Matrix:")
    sep_line = "  " + "-" * 90
    print(sep_line)
    hdr = f"  {'Protocol':<12} {'n_swaps':>8} {'Same-pool':>11} {'Intra-proto':>13} {'Cross-proto':>13}"
    print(hdr)
    print(sep_line)
    for amm in PROTO_ORDER:
        if total[amm] == 0:
            continue
        t = total[amm]
        sp = same_pool[amm]
        ip = intra_proto[amm]
        cp = t - sp - ip
        label = PROTOCOL_LABELS.get(amm, amm)
        print(
            f"  {label:<12} {t:>8} {100 * sp / t:>10.1f}% {100 * ip / t:>12.1f}% {100 * cp / t:>12.1f}%"
        )

        if cp > 0:
            parts = []
            for dest in PROTO_ORDER:
                if dest != amm and cross_proto[amm][dest] > 0:
                    dest_label = PROTOCOL_LABELS.get(dest, dest)
                    dest_pct = 100 * cross_proto[amm][dest] / t
                    if dest_pct >= 0.05:
                        parts.append(f"{dest_label} {dest_pct:.1f}%")
            if parts:
                breakdown = ", ".join(parts)
                print(f"  {'':>34}  cross breakdown: {breakdown}")
    print(sep_line)

    row_amms = [a for a in PROTO_ORDER if total[a] > 0]
    col_amms = PROTO_ORDER
    n_rows = len(row_amms)
    n_cols = len(col_amms)

    mat = np.zeros((n_rows, n_cols))
    for i, ra in enumerate(row_amms):
        t = total[ra]
        for j, ca in enumerate(col_amms):
            if ra == ca:
                mat[i, j] = 100.0 * (same_pool[ra] + intra_proto[ra]) / t
            else:
                mat[i, j] = 100.0 * cross_proto[ra][ca] / t

    intra_pct = {}
    same_pct_dict = {}
    for amm in row_amms:
        t = total[amm]
        intra_pct[amm] = 100.0 * intra_proto[amm] / t if t > 0 else 0.0
        same_pct_dict[amm] = 100.0 * same_pool[amm] / t if t > 0 else 0.0

    row_labels = [PROTOCOL_LABELS.get(a, a) for a in row_amms]
    col_labels = [PROTOCOL_LABELS.get(a, a) for a in col_amms]

    fig, ax = plt.subplots(figsize=(8.0, 4.5))
    cmap = plt.cm.YlOrRd
    im = ax.imshow(mat, cmap=cmap, vmin=0, vmax=90, aspect="auto")

    ax.set_xticks(range(n_cols))
    ax.set_xticklabels(col_labels, fontsize=9, rotation=30, ha="right")
    ax.set_yticks(range(n_rows))
    ax.set_yticklabels(row_labels, fontsize=9)
    ax.set_xlabel("Best alternative protocol")
    ax.set_ylabel("Executed at protocol")

    cbar = fig.colorbar(im, ax=ax, shrink=0.85, pad=0.02)
    cbar.set_label("% of swaps", fontsize=9)

    for i in range(n_rows):
        ra = row_amms[i]
        for j in range(n_cols):
            ca = col_amms[j]
            val = mat[i, j]

            if ra == ca:
                ip = intra_pct[ra]
                sp = same_pct_dict[ra]
                diag_total = val
                text_color = "white" if diag_total > 50 else "black"

                if ip > 0.5:
                    ax.text(
                        j,
                        i - 0.18,
                        f"{diag_total:.0f}%",
                        ha="center",
                        va="center",
                        fontsize=11,
                        fontweight="bold",
                        color=text_color,
                    )
                    ax.text(
                        j,
                        i + 0.18,
                        f"({sp:.0f}% + {ip:.0f}% intra)",
                        ha="center",
                        va="center",
                        fontsize=6.5,
                        color=text_color,
                        fontstyle="italic",
                    )
                else:
                    ax.text(
                        j,
                        i,
                        f"{diag_total:.0f}%",
                        ha="center",
                        va="center",
                        fontsize=11,
                        fontweight="bold",
                        color=text_color,
                    )
            else:
                if val >= 0.05:
                    text_color = "white" if val > 50 else "black"
                    ax.text(
                        j,
                        i,
                        f"{val:.1f}%",
                        ha="center",
                        va="center",
                        fontsize=8.5,
                        color=text_color,
                    )
                else:
                    ax.text(j, i, "\u2013", ha="center", va="center", fontsize=9, color="0.6")

    fig.tight_layout()
    path = os.path.join(out_dir, "plot_routing_matrix.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"  Saved: {path}")

    csv_rows = []
    for amm in row_amms:
        t = total[amm]
        row_dict = {
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "n_swaps": t,
            "same_pool_pct": round(100 * same_pool[amm] / t, 1),
            "intra_protocol_pct": round(100 * intra_proto[amm] / t, 1),
            "diagonal_total_pct": round(100 * (same_pool[amm] + intra_proto[amm]) / t, 1),
        }
        for dest in col_amms:
            dest_label = PROTOCOL_LABELS.get(dest, dest)
            if dest != amm:
                row_dict[dest_label] = round(100 * cross_proto[amm][dest] / t, 1)
            else:
                row_dict[dest_label] = round(100 * (same_pool[amm] + intra_proto[amm]) / t, 1)
        csv_rows.append(row_dict)

    df_out = pl.DataFrame(csv_rows)
    csv_path = os.path.join(out_dir, "table_routing_matrix.csv")
    df_out.write_csv(csv_path)
    print(f"  Saved: {csv_path}")
    print(df_out)


def section_execution(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    _event_study(session_dir, metadata, pyth_sol, out_dir)
    _exec_slippage(session_dir, metadata, pyth_sol, out_dir)
    _realized_slippage_scatter(session_dir, metadata, pyth_sol, out_dir)


def _event_study(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.5: Event Study\n{SEP}")

    WINDOW = 15
    pools = metadata["pools"]

    for pool in pools:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS:
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        if states.height == 0 or quotes.height == 0:
            continue

        swaps = detect_swaps(states)
        if swaps.height < 20:
            continue

        swap_ts_set = set(swaps["timestamp_ms"].to_list())

        paired = compute_bid_ask(quotes, 100.0)
        if paired.height < 50:
            continue

        ts_arr = paired["timestamp_ms"].to_numpy()
        pyth_prices = pyth_sol.vec(ts_arr)
        bid_arr = paired["bid"].to_numpy()
        ask_arr = paired["ask"].to_numpy()
        spread_arr = paired["spread_bps"].to_numpy()
        bid_vs_pyth = (bid_arr - pyth_prices) / pyth_prices * 10000
        ask_vs_pyth = (ask_arr - pyth_prices) / pyth_prices * 10000

        is_swap = np.array([t in swap_ts_set for t in ts_arr])
        swap_indices = np.where(is_swap)[0]

        spread_windows = []
        bid_windows = []
        ask_windows = []
        for idx in swap_indices:
            if idx < WINDOW or idx >= len(spread_arr) - WINDOW:
                continue
            w_spread = spread_arr[idx - WINDOW : idx + WINDOW + 1]
            w_bid = bid_vs_pyth[idx - WINDOW : idx + WINDOW + 1]
            w_ask = ask_vs_pyth[idx - WINDOW : idx + WINDOW + 1]
            if np.any(np.isnan(w_spread)) or np.any(np.isnan(w_bid)):
                continue
            spread_windows.append(w_spread)
            bid_windows.append(w_bid)
            ask_windows.append(w_ask)

        if len(spread_windows) < 20:
            continue

        spread_mat = np.array(spread_windows)
        bid_mat = np.array(bid_windows)
        ask_mat = np.array(ask_windows)
        n_w = len(spread_windows)

        mean_spread = np.mean(spread_mat, axis=0)
        std_spread = np.std(spread_mat, axis=0) / np.sqrt(n_w)
        mean_bid = np.mean(bid_mat, axis=0)
        mean_ask = np.mean(ask_mat, axis=0)
        offsets = list(range(-WINDOW, WINDOW + 1))

        label = PROTOCOL_LABELS.get(amm, amm)
        pool_short = pool["pool_id"][:12]
        print(f"  {label} ({pool_short}): {n_w} swap windows")

        fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(10, 6), sharex=True)

        ax1.plot(offsets, mean_spread, color="purple", linewidth=1.5, label="mean spread")
        ax1.fill_between(
            offsets,
            mean_spread - 2 * std_spread,
            mean_spread + 2 * std_spread,
            alpha=0.2,
            color="purple",
            label=r"$\pm 2$ SE",
        )
        ax1.axvline(x=0, color="green", linewidth=1, alpha=0.5, linestyle="--", label="swap")
        ax1.set_ylabel("Spread (bps)")
        ax1.legend(fontsize=7)
        ax1.set_title(f"{label}: Average Spread Around Swaps (n={n_w})")

        ax2.plot(offsets, mean_bid, color="blue", linewidth=1, label="bid vs Pyth")
        ax2.plot(offsets, mean_ask, color="red", linewidth=1, label="ask vs Pyth")
        ax2.axhline(y=0, color="black", linewidth=0.5)
        ax2.axvline(x=0, color="green", linewidth=1, alpha=0.5, linestyle="--")
        ax2.set_ylabel("vs Pyth (bps)")
        ax2.set_xlabel("State updates relative to swap")
        ax2.legend(fontsize=7)

        fig.tight_layout()
        path = os.path.join(out_dir, f"event_study_{amm}_{pool_short}.pdf")
        fig.savefig(path)
        plt.close(fig)
        print(f"  Saved: {path}")


def _exec_slippage(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.5: Execution Slippage\n{SEP}")

    proto_slippage = defaultdict(list)

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS:
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        if states.height == 0 or quotes.height == 0:
            continue

        swaps = detect_swaps(states)
        if swaps.height == 0:
            continue

        swap_details = swaps.with_columns(
            (
                pl.col("qd").abs().cast(pl.Float64)
                / pl.col("bd").abs().cast(pl.Float64)
                * PRICE_SCALE
            ).alias("exec_price")
        )

        paired = compute_bid_ask(quotes, 100.0)
        if paired.height < 10:
            continue

        ba_ts = paired["timestamp_ms"].to_numpy()
        ba_bids = paired["bid"].to_numpy()
        ba_asks = paired["ask"].to_numpy()

        for row in swap_details.iter_rows(named=True):
            exec_px = row["exec_price"]
            if exec_px < 10 or exec_px > 500:
                continue

            swap_ts = row["timestamp_ms"]
            is_b2q = row["bd"] > 0

            qidx = np.searchsorted(ba_ts, swap_ts, side="right") - 1
            if qidx < 0 or ba_ts[qidx] >= swap_ts:
                continue

            if is_b2q:
                quoted = ba_bids[qidx]
                if quoted > 0:
                    slip = (quoted - exec_px) / quoted * 10000
                    proto_slippage[amm].append(slip)
            else:
                quoted = ba_asks[qidx]
                if quoted > 0:
                    slip = (exec_px - quoted) / quoted * 10000
                    proto_slippage[amm].append(slip)

    amms_with_data = sorted([a for a in proto_slippage if len(proto_slippage[a]) > 10])
    if not amms_with_data:
        print("  No slippage data, skipping")
        return

    ncols = min(3, len(amms_with_data))
    nrows = (len(amms_with_data) + ncols - 1) // ncols
    fig, axes = plt.subplots(nrows, ncols, figsize=(4.5 * ncols, 3.5 * nrows), squeeze=False)

    for idx, amm in enumerate(amms_with_data):
        ax = axes[idx // ncols, idx % ncols]
        vals = np.array(proto_slippage[amm])
        p1, p99 = np.percentile(vals, [1, 99])
        clipped = vals[(vals >= p1) & (vals <= p99)]

        if len(clipped) > 0:
            ax.hist(
                clipped,
                bins=50,
                color=PROTOCOL_COLORS.get(amm, "gray"),
                alpha=0.7,
                edgecolor="black",
                linewidth=0.3,
            )
            ax.axvline(
                np.mean(vals),
                color="red",
                linewidth=1.5,
                linestyle="--",
                label=f"mean={np.mean(vals):.1f} bps",
            )
            ax.axvline(0, color="black", linewidth=0.5, linestyle=":")

        ax.set_xlabel("Slippage (bps)")
        ax.set_ylabel("Count")
        ax.set_title(f"{PROTOCOL_LABELS.get(amm, amm)} (n={len(vals)})")
        ax.legend(loc="upper right", fontsize=7)
        ax.grid(True, alpha=0.3)

    for idx in range(len(amms_with_data), nrows * ncols):
        axes[idx // ncols, idx % ncols].set_visible(False)

    fig.suptitle("Execution Slippage (positive = worse for swapper)", fontsize=11)
    fig.tight_layout()
    path = os.path.join(out_dir, "plot_exec_slippage.pdf")
    fig.savefig(path)
    plt.close(fig)
    print(f"Saved: {path}")

    rows = []
    for amm in sorted(proto_slippage.keys()):
        vals = np.array(proto_slippage[amm])
        if len(vals) == 0:
            continue
        rows.append({
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "n_swaps": len(vals),
            "pct_worse_than_quoted": round(100 * np.sum(vals > 0) / len(vals), 1),
            "mean_slippage_bps": round(np.mean(vals), 2),
            "median_slippage_bps": round(np.median(vals), 2),
        })
    if rows:
        df_out = pl.DataFrame(rows)
        path = os.path.join(out_dir, "table_exec_quality.csv")
        df_out.write_csv(path)
        print(df_out)
        print(f"Saved: {path}")


def section_jitter(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nJITTER ANALYSIS (Supplementary)\n{SEP}")

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if quotes.height == 0 or states.height == 0:
            continue

        if amm in SKIP_SWAP_PROTOCOLS:
            swap_ts_set = set()
        else:
            swaps = detect_swaps(states)
            swap_ts_set = set(swaps["timestamp_ms"].to_list()) if swaps.height > 0 else set()

        paired = compute_bid_ask(quotes, 100.0)
        if paired.height < 10:
            continue

        ts_arr = paired["timestamp_ms"].to_numpy()
        pyth_prices = pyth_sol.vec(ts_arr)

        b2q_all = quotes.filter(
            (pl.col("direction") == "B2Q")
            & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1)
            & (pl.col("slot") > 0)
        )
        q2b_all = quotes.filter(
            (pl.col("direction") == "Q2B")
            & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1)
            & (pl.col("slot") > 0)
        )
        n_zero_bid = b2q_all.filter(pl.col("output_amount") == 0).height
        n_zero_ask = q2b_all.filter(pl.col("output_amount") == 0).height

        bid_arr = paired["bid"].to_numpy()
        ask_arr = paired["ask"].to_numpy()
        spread_arr = paired["spread_bps"].to_numpy()
        is_swap = np.array([t in swap_ts_set for t in ts_arr])

        label = PROTOCOL_LABELS.get(amm, amm)
        pool_short = pool["pool_id"][:12]
        n_swaps = is_swap.sum()

        print(f"\n  {label} ({pool_short}): {len(ts_arr)} updates, {n_swaps} swaps")
        print(f"    Zero quotes: bid={n_zero_bid}, ask={n_zero_ask}")
        if paired.height > 0:
            print(
                f"    Spread: mean={np.nanmean(spread_arr):.2f} bps, std={np.nanstd(spread_arr):.2f}"
            )

        sub_n = min(800, len(ts_arr))
        t0 = ts_arr[0]
        times = (ts_arr[:sub_n] - t0) / 1000

        fig, axes_arr = plt.subplots(
            3, 1, figsize=(12, 7), sharex=True, gridspec_kw={"height_ratios": [4, 1.2, 1]}
        )
        ax1, ax2, ax3 = axes_arr

        ax1.plot(times, bid_arr[:sub_n], color="blue", alpha=0.7, linewidth=0.3, label="bid")
        ax1.plot(times, ask_arr[:sub_n], color="red", alpha=0.7, linewidth=0.3, label="ask")
        ax1.plot(times, pyth_prices[:sub_n], color="black", linewidth=0.6, alpha=0.8, label="Pyth")

        fill_mask = np.isfinite(bid_arr[:sub_n]) & np.isfinite(ask_arr[:sub_n])
        if fill_mask.any():
            ax1.fill_between(
                times, bid_arr[:sub_n], ask_arr[:sub_n], where=fill_mask, alpha=0.08, color="gray"
            )

        swap_times = times[is_swap[:sub_n]]
        n_swap_window = len(swap_times)
        for i, st in enumerate(swap_times):
            kw = {"label": f"swap ({n_swap_window})"} if i == 0 else {}
            ax1.axvline(x=st, color="green", alpha=0.3, linewidth=0.8, **kw)

        ax1.set_ylabel("Price (USD/SOL)")
        ax1.legend(fontsize=6, loc="upper right")
        ax1.set_title(f"{label}: Per-Update Bid/Ask vs Pyth (USD 100 tier, {sub_n} updates)")

        ax2.plot(times, spread_arr[:sub_n], color="purple", linewidth=0.4)
        ax2.set_ylabel("Spread (bps)")
        ax2.axhline(y=0, color="black", linewidth=0.3)
        for st in swap_times:
            ax2.axvline(x=st, color="green", alpha=0.2, linewidth=0.5)

        if swap_times.size > 0:
            ax3.eventplot(
                [swap_times.tolist()],
                lineoffsets=0.5,
                linelengths=0.8,
                colors="red",
                linewidths=0.6,
            )
        ax3.set_ylabel("Swaps")
        ax3.set_xlabel("Time (seconds)")
        ax3.set_yticks([])

        fig.tight_layout()
        path = os.path.join(out_dir, f"jitter_{amm}_{pool_short}.pdf")
        fig.savefig(path)
        plt.close(fig)
        print(f"    Saved: {path}")


def section_microstructure(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    _realized_spread(session_dir, metadata, pyth_sol, out_dir)
    _kyles_lambda(session_dir, metadata, pyth_sol, out_dir)
    _order_flow_toxicity(session_dir, metadata, pyth_sol, out_dir)
    _inventory_mean_reversion(session_dir, metadata, pyth_sol, out_dir)
    _microstructure_plots(session_dir, metadata, pyth_sol, out_dir)


def _realized_spread(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nMICROSTRUCTURE: Realized Spread\n{SEP}")
    import numpy as np

    horizons_ms = [5000, 30000]
    horizon_labels = ["5s", "30s"]
    rows = []

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        bcol, qcol = "base_vault_balance", "quote_vault_balance"
        if bcol not in states.columns:
            continue

        swaps = detect_swaps(states)
        if swaps.height < 10:
            continue

        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        bd = swaps["bd"].to_numpy().astype(np.float64) / 1e9
        qd = swaps["qd"].to_numpy().astype(np.float64) / 1e6

        exec_price = np.abs(qd / bd)
        direction = np.sign(bd)

        mid_at_swap = pyth_sol.vec(swap_ts)

        quoted_hs_bps = direction * (exec_price - mid_at_swap) / mid_at_swap * 10000

        for h_idx, h_ms in enumerate(horizons_ms):
            mid_post = pyth_sol.vec(swap_ts + h_ms)
            adverse_bps = direction * (mid_post - mid_at_swap) / mid_at_swap * 10000
            realized_bps = quoted_hs_bps - adverse_bps

            rows.append({
                "protocol": PROTOCOL_LABELS.get(amm, amm),
                "pool_id": pool["pool_id"][:12],
                "horizon": horizon_labels[h_idx],
                "n_swaps": len(bd),
                "mean_quoted_hs_bps": round(float(np.nanmean(quoted_hs_bps)), 3),
                "mean_adverse_bps": round(float(np.nanmean(adverse_bps)), 3),
                "mean_realized_bps": round(float(np.nanmean(realized_bps)), 3),
                "median_realized_bps": round(float(np.nanmedian(realized_bps)), 3),
            })

    if rows:
        df_out = pl.DataFrame(rows)
        agg = (
            df_out
            .group_by(["protocol", "horizon"])
            .agg([
                pl.col("n_swaps").sum(),
                pl.col("mean_quoted_hs_bps").mean().alias("quoted_hs_bps"),
                pl.col("mean_adverse_bps").mean().alias("adverse_bps"),
                pl.col("mean_realized_bps").mean().alias("realized_bps"),
            ])
            .sort(["protocol", "horizon"])
        )
        print(agg)
        path_csv = os.path.join(out_dir, "table_realized_spread.csv")
        df_out.write_csv(path_csv)
        print(f"Saved: {path_csv}")
    else:
        print("  No data for realized spread")


def _kyles_lambda(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nMICROSTRUCTURE: Kyle's Lambda\n{SEP}")
    import numpy as np
    from scipy import stats as sp_stats

    window_ms = 30_000
    rows = []

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        swaps = detect_swaps(states)
        if swaps.height < 20:
            continue

        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        bd = swaps["bd"].to_numpy().astype(np.float64) / 1e9
        qd = swaps["qd"].to_numpy().astype(np.float64) / 1e6

        mid_at_swap = pyth_sol.vec(swap_ts)
        signed_flow_usd = bd * mid_at_swap

        t0 = swap_ts[0]
        t_end = swap_ts[-1]
        n_buckets = int((t_end - t0) / window_ms) + 1
        if n_buckets < 10:
            continue

        bucket_flow = np.zeros(n_buckets)
        bucket_dp = np.zeros(n_buckets)
        for i in range(n_buckets):
            w_start = t0 + i * window_ms
            w_end = w_start + window_ms
            mask = (swap_ts >= w_start) & (swap_ts < w_end)
            bucket_flow[i] = signed_flow_usd[mask].sum()
            p_start = pyth_sol.at(w_start)
            p_end = pyth_sol.at(w_end)
            bucket_dp[i] = (p_end - p_start) / p_start * 10000

        nonzero = bucket_flow != 0
        if nonzero.sum() < 10:
            continue

        slope, intercept, r_value, p_value, std_err = sp_stats.linregress(
            bucket_flow[nonzero], bucket_dp[nonzero]
        )

        rows.append({
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "pool_id": pool["pool_id"][:12],
            "lambda_bps_per_1k": round(slope * 1000, 4),
            "r_squared": round(r_value**2, 4),
            "p_value": round(p_value, 6),
            "n_windows": int(nonzero.sum()),
        })

    if rows:
        df_out = pl.DataFrame(rows)
        print(df_out)
        path_csv = os.path.join(out_dir, "table_kyles_lambda.csv")
        df_out.write_csv(path_csv)
        print(f"Saved: {path_csv}")
    else:
        print("  No data for Kyle's lambda")


def _order_flow_toxicity(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nMICROSTRUCTURE: Order Flow Toxicity\n{SEP}")
    import numpy as np

    horizon_ms = 5000
    rows = []

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        swaps = detect_swaps(states)
        if swaps.height < 10:
            continue

        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        bd = swaps["bd"].to_numpy().astype(np.float64) / 1e9
        qd = swaps["qd"].to_numpy().astype(np.float64) / 1e6

        direction = np.sign(bd)
        volume_usd = np.abs(qd)

        mid_at_swap = pyth_sol.vec(swap_ts)
        mid_after = pyth_sol.vec(swap_ts + horizon_ms)
        price_move = (mid_after - mid_at_swap) / mid_at_swap * 10000

        informed = (direction * price_move) < 0

        n_total = len(informed)
        n_informed = informed.sum()
        vol_total = volume_usd.sum()
        vol_informed = volume_usd[informed].sum()

        rows.append({
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "pool_id": pool["pool_id"][:12],
            "n_swaps": n_total,
            "pct_informed_count": round(n_informed / n_total * 100, 1),
            "pct_informed_volume": round(vol_informed / vol_total * 100, 1) if vol_total > 0 else 0,
            "mean_adverse_move_bps": round(float(np.mean(np.abs(price_move[informed]))), 2)
            if n_informed > 0
            else 0,
            "mean_favorable_move_bps": round(float(np.mean(np.abs(price_move[~informed]))), 2)
            if (~informed).sum() > 0
            else 0,
        })

    if rows:
        df_out = pl.DataFrame(rows)
        agg = (
            df_out
            .group_by("protocol")
            .agg([
                pl.col("n_swaps").sum(),
                pl.col("pct_informed_count").mean().alias("pct_informed_count"),
                pl.col("pct_informed_volume").mean().alias("pct_informed_volume"),
                pl.col("mean_adverse_move_bps").mean(),
                pl.col("mean_favorable_move_bps").mean(),
            ])
            .sort("protocol")
        )
        print(agg)
        path_csv = os.path.join(out_dir, "table_toxicity.csv")
        df_out.write_csv(path_csv)
        print(f"Saved: {path_csv}")
    else:
        print("  No data for toxicity")


def _inventory_mean_reversion(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nMICROSTRUCTURE: Inventory Mean Reversion\n{SEP}")
    import numpy as np
    from scipy import stats as sp_stats

    target_amms = ["humidifi", "bisonfi", "solfiv2", "goonfi", "zerofi", "alphaq"]
    rows = []

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm not in target_amms:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        bcol, qcol = "base_vault_balance", "quote_vault_balance"
        if bcol not in states.columns:
            continue

        clean = (
            states
            .filter(pl.col("slot") > 0)
            .filter((pl.col(bcol) > 0) & (pl.col(bcol) < MAX_VAULT))
            .filter((pl.col(qcol) > 0) & (pl.col(qcol) < MAX_VAULT))
            .sort(["slot", "write_version"])
        )

        if clean.height <= 100:
            continue
        clean = clean.slice(50)

        ts = clean["timestamp_ms"].to_numpy().astype(np.float64)
        base = clean[bcol].to_numpy().astype(np.float64)
        quote = clean[qcol].to_numpy().astype(np.float64)
        prices = pyth_sol.vec(ts)

        base_usd = base / 1e9 * prices
        quote_usd = quote / 1e6
        tvl = base_usd + quote_usd
        mask = tvl > 0
        q = np.full_like(tvl, np.nan)
        q[mask] = (base_usd[mask] - quote_usd[mask]) / tvl[mask]

        step = max(1, clean.height // 10000)
        q_sub = q[::step]
        q_sub = q_sub[~np.isnan(q_sub)]

        if len(q_sub) < 50:
            continue

        y = q_sub[1:]
        x = q_sub[:-1]
        slope, intercept, r_value, p_value, _ = sp_stats.linregress(x, y)

        dt_sec = step * (ts[-1] - ts[0]) / len(ts) / 1000.0
        if 0 < slope < 1:
            half_life_samples = -np.log(2) / np.log(slope)
            half_life_sec = half_life_samples * dt_sec
        else:
            half_life_sec = float("inf")

        rows.append({
            "protocol": PROTOCOL_LABELS.get(amm, amm),
            "pool_id": pool["pool_id"][:12],
            "ar1_beta": round(slope, 6),
            "r_squared": round(r_value**2, 4),
            "half_life_sec": round(half_life_sec, 1) if half_life_sec < 1e6 else float("inf"),
            "mean_q": round(float(np.nanmean(q)), 4),
            "std_q": round(float(np.nanstd(q)), 4),
        })

    if rows:
        df_out = pl.DataFrame(rows)
        print(df_out)
        path_csv = os.path.join(out_dir, "table_mean_reversion.csv")
        df_out.write_csv(path_csv)
        print(f"Saved: {path_csv}")
    else:
        print("  No data for mean reversion")


def _microstructure_plots(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    import numpy as np

    rs_path = os.path.join(out_dir, "table_realized_spread.csv")
    if os.path.exists(rs_path):
        rs = pl.read_csv(rs_path)
        rs5 = rs.filter(pl.col("horizon") == "5s")
        if rs5.height > 0:
            agg = (
                rs5
                .group_by("protocol")
                .agg([
                    pl.col("mean_quoted_hs_bps").mean().alias("quoted"),
                    pl.col("mean_adverse_bps").mean().alias("adverse"),
                    pl.col("mean_realized_bps").mean().alias("realized"),
                ])
                .sort("protocol")
            )

            protocols = agg["protocol"].to_list()
            quoted = agg["quoted"].to_numpy()
            adverse = agg["adverse"].to_numpy()
            realized = agg["realized"].to_numpy()

            fig, ax = plt.subplots(figsize=(8, 4))
            x = np.arange(len(protocols))
            w = 0.25
            ax.bar(x - w, quoted, w, label="Quoted half-spread", color="steelblue")
            ax.bar(x, adverse, w, label="Adverse selection (5s)", color="indianred")
            ax.bar(x + w, realized, w, label="Realized spread", color="seagreen")
            ax.set_xticks(x)
            ax.set_xticklabels(protocols, fontsize=8)
            ax.set_ylabel("Basis points")
            ax.set_title("Spread Decomposition: Quoted vs Adverse Selection vs Realized (5s)")
            ax.axhline(0, color="black", linewidth=0.5)
            ax.legend(fontsize=7)
            ax.grid(True, alpha=0.3, axis="y")
            fig.tight_layout()
            fig.savefig(os.path.join(out_dir, "plot_realized_spread.pdf"))
            plt.close(fig)
            print(f"Saved: {os.path.join(out_dir, 'plot_realized_spread.pdf')}")

    tox_path = os.path.join(out_dir, "table_toxicity.csv")
    if os.path.exists(tox_path):
        tox = pl.read_csv(tox_path)
        agg = (
            tox
            .group_by("protocol")
            .agg([
                pl.col("pct_informed_count").mean(),
                pl.col("pct_informed_volume").mean(),
            ])
            .sort("pct_informed_volume")
        )

        fig, ax = plt.subplots(figsize=(7, 3.5))
        protocols = agg["protocol"].to_list()
        y = np.arange(len(protocols))
        count_pct = agg["pct_informed_count"].to_numpy()
        vol_pct = agg["pct_informed_volume"].to_numpy()
        h = 0.35
        ax.barh(y - h / 2, count_pct, h, label="% informed (count)", color="steelblue", alpha=0.8)
        ax.barh(y + h / 2, vol_pct, h, label="% informed (volume)", color="indianred", alpha=0.8)
        ax.set_yticks(y)
        ax.set_yticklabels(protocols, fontsize=9)
        ax.set_xlabel("Percentage of trades classified as informed")
        ax.set_title("Order Flow Toxicity by Protocol (5s horizon)")
        ax.axvline(50, color="black", linewidth=0.5, linestyle="--", alpha=0.5)
        ax.legend(fontsize=7, loc="lower right")
        ax.grid(True, alpha=0.3, axis="x")
        fig.tight_layout()
        fig.savefig(os.path.join(out_dir, "plot_toxicity.pdf"))
        plt.close(fig)
        print(f"Saved: {os.path.join(out_dir, 'plot_toxicity.pdf')}")

    mr_path = os.path.join(out_dir, "table_mean_reversion.csv")
    if os.path.exists(mr_path):
        mr = pl.read_csv(mr_path)
        mr = mr.filter(pl.col("half_life_sec") < 1e6).sort("half_life_sec")

        fig, ax = plt.subplots(figsize=(8, 4))
        labels = ["%s\n%s" % (r["protocol"], r["pool_id"][:8]) for r in mr.iter_rows(named=True)]
        hl = mr["half_life_sec"].to_numpy()
        colors = [
            PROTOCOL_COLORS.get(r["protocol"].lower().replace(" v2", "").replace(" ", ""), "gray")
            for r in mr.iter_rows(named=True)
        ]
        label_to_key = {v: k for k, v in PROTOCOL_LABELS.items()}
        colors = []
        for r in mr.iter_rows(named=True):
            key = label_to_key.get(r["protocol"], r["protocol"].lower())
            colors.append(PROTOCOL_COLORS.get(key, "gray"))

        ax.bar(range(len(hl)), hl, color=colors, alpha=0.8)
        ax.set_xticks(range(len(hl)))
        ax.set_xticklabels(labels, fontsize=7, rotation=45, ha="right")
        ax.set_ylabel("Half-life (seconds)")
        ax.set_yscale("log")
        ax.set_title("Inventory Mean Reversion Half-Life (AR(1) model)")
        ax.grid(True, alpha=0.3, axis="y")
        fig.tight_layout()
        fig.savefig(os.path.join(out_dir, "plot_mean_reversion.pdf"))
        plt.close(fig)
        print(f"Saved: {os.path.join(out_dir, 'plot_mean_reversion.pdf')}")


def _quote_update_rate(session_dir: str, metadata: dict, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.2: Quote Update Rate\n{SEP}")

    swap_rates = {}
    reprice_rates = {}
    bcol, qcol = "base_vault_balance", "quote_vault_balance"

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in EXCLUDE_CROSS_COMPARE:
            continue
        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height < 50:
            continue
        real = states.filter(pl.col("slot") > 0).sort(["slot", "write_version"])
        if real.height < 50 or bcol not in real.columns:
            continue

        ts = real["timestamp_ms"].to_numpy().astype(np.float64)
        dur_h = (ts[-1] - ts[0]) / 3.6e6
        if dur_h < 0.1:
            continue

        base = real[bcol].to_numpy()
        quote = real[qcol].to_numpy()
        db = np.diff(base)
        dq = np.diff(quote)

        vault_changed = (db != 0) | (dq != 0)
        n_vault = vault_changed.sum()

        n_total = len(db)
        n_noop = n_total - n_vault

        label = PROTOCOL_LABELS.get(amm, amm)
        swap_r = n_vault / dur_h
        reprice_r = n_noop / dur_h

        if label not in swap_rates or swap_r > swap_rates[label]:
            swap_rates[label] = swap_r
            reprice_rates[label] = reprice_r

    if not swap_rates:
        print("  No data")
        return

    sorted_labels = sorted(swap_rates.keys(), key=lambda l: swap_rates[l], reverse=True)
    swap_vals = [swap_rates[l] for l in sorted_labels]
    reprice_vals = [reprice_rates[l] for l in sorted_labels]

    label_to_key = {v: k for k, v in PROTOCOL_LABELS.items()}
    colors = [PROTOCOL_COLORS.get(label_to_key.get(l, l.lower()), "gray") for l in sorted_labels]

    fig, ax = plt.subplots(figsize=(7, 3.5))
    y = range(len(sorted_labels))
    bars1 = ax.barh(y, swap_vals, color=colors, alpha=0.9, label="Vault-changing (swaps)")
    bars2 = ax.barh(
        y, reprice_vals, left=swap_vals, color=colors, alpha=0.3, label="Oracle-only repricing"
    )
    ax.set_yticks(y)
    ax.set_yticklabels(sorted_labels, fontsize=9)
    ax.set_xlabel("Updates per hour")
    ax.set_title("State Update Frequency by Protocol")
    ax.invert_yaxis()
    ax.legend(fontsize=7, loc="lower right")
    ax.grid(True, alpha=0.3, axis="x")

    for bar, sv, rv in zip(bars1, swap_vals, reprice_vals):
        total = sv + rv
        ax.text(
            total + max(s + r for s, r in zip(swap_vals, reprice_vals)) * 0.01,
            bar.get_y() + bar.get_height() / 2,
            f"{sv:,.0f} + {rv:,.0f}",
            va="center",
            fontsize=6,
        )

    fig.tight_layout()
    path_out = os.path.join(out_dir, "plot_update_rate.pdf")
    fig.savefig(path_out)
    plt.close(fig)
    print(f"Saved: {path_out}")


def _spread_tiers(
    session_dir: str, metadata: dict, pyth_sol: PythIndex, pyth_df: pl.DataFrame, out_dir: str
):
    print(f"\n{SEP}\nSECTION 7.3.2: Spread by Tier\n{SEP}")

    tiers = [1.0, 100.0, 1000.0, 10000.0]
    tier_labels = ["USD 1", "USD 100", "USD 1000", "USD 10000"]

    proto_spreads = {}

    pools = metadata["pools"]
    primary = {}
    for pool in pools:
        amm = pool["amm_type"]
        if amm in EXCLUDE_CROSS_COMPARE:
            continue
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        n = quotes.filter(pl.col("slot") > 0).height if quotes.height > 0 else 0
        if amm not in primary or n > primary[amm][1]:
            primary[amm] = (pool, n)

    for amm, (pool, _) in primary.items():
        quotes = load_pool_data(session_dir, pool["data_dir"], "quotes")
        if quotes.height < 100:
            continue
        label = PROTOCOL_LABELS.get(amm, amm)
        spreads = []
        for tier in tiers:
            paired = compute_bid_ask(quotes, tier)
            if paired.height < 10:
                spreads.append(np.nan)
            else:
                spreads.append(float(paired["spread_bps"].median()))
        proto_spreads[label] = spreads

    if not proto_spreads:
        print("  No data")
        return

    sorted_protos = sorted(
        proto_spreads.keys(),
        key=lambda p: proto_spreads[p][1] if not np.isnan(proto_spreads[p][1]) else 999,
    )

    fig, ax = plt.subplots(figsize=(9, 4))
    n_protos = len(sorted_protos)
    n_tiers = len(tiers)
    x = np.arange(n_protos)
    width = 0.8 / n_tiers
    tier_colors = ["#2196F3", "#4CAF50", "#FF9800", "#F44336"]

    for t_idx, (tier_label, color) in enumerate(zip(tier_labels, tier_colors)):
        vals = [proto_spreads[p][t_idx] for p in sorted_protos]
        offset = (t_idx - n_tiers / 2 + 0.5) * width
        bars = ax.bar(x + offset, vals, width, label=tier_label, color=color, alpha=0.8)

    ax.set_xticks(x)
    ax.set_xticklabels(sorted_protos, fontsize=8)
    ax.set_ylabel("Median spread (bps)")
    ax.set_yscale("log")
    ax.set_title("Median Spread by Trade Size and Protocol")
    ax.legend(fontsize=7, loc="upper left")
    ax.grid(True, alpha=0.3, axis="y")
    fig.tight_layout()
    path_out = os.path.join(out_dir, "plot_spread_tiers.pdf")
    fig.savefig(path_out)
    plt.close(fig)
    print(f"Saved: {path_out}")


def _realized_slippage_scatter(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION 7.3.6: Realized Slippage vs Size\n{SEP}")

    import glob as _glob

    from matplotlib.ticker import LogLocator, NullFormatter

    _q_files = sorted(
        _glob.glob(os.path.join(session_dir, "quoter_output", "SOL-USDC", "quotes_*.parquet"))
    )
    if not _q_files:
        print("  No quote files")
        return
    _quotes = pl.concat([pl.read_parquet(f) for f in _q_files])
    _quotes = _quotes.filter(pl.col("slot") > 0).filter(pl.col("output_amount") > 0)
    _PS = 1e3

    _b2q = (
        _quotes
        .filter((pl.col("direction") == "B2Q") & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1))
        .with_columns(
            (
                pl.col("output_amount").cast(pl.Float64)
                / pl.col("input_amount").cast(pl.Float64)
                * _PS
            ).alias("bid")
        )
        .select(["timestamp_ms", "slot", "pool_id", "bid"])
    )
    _q2b = (
        _quotes
        .filter((pl.col("direction") == "Q2B") & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1))
        .with_columns(
            (
                pl.col("input_amount").cast(pl.Float64)
                / pl.col("output_amount").cast(pl.Float64)
                * _PS
            ).alias("ask")
        )
        .select(["slot", "pool_id", "ask"])
    )
    _mids = (
        _b2q
        .join(_q2b, on=["slot", "pool_id"], how="inner")
        .with_columns(((pl.col("bid") + pl.col("ask")) / 2).alias("mid"))
        .select(["timestamp_ms", "slot", "pool_id", "mid"])
        .sort("timestamp_ms")
    )

    _pool_ids = _mids["pool_id"].unique().sort().to_list()
    _pool_map = {pid: i for i, pid in enumerate(_pool_ids)}
    _n_pools = len(_pool_ids)
    _ts_arr = _mids["timestamp_ms"].to_numpy().astype(np.float64)
    _mid_arr = _mids["mid"].to_numpy()
    _pidx_arr = np.array([_pool_map[p] for p in _mids["pool_id"].to_list()])

    _latest = np.full(_n_pools, np.nan)
    _con_ts_list = []
    _con_px_list = []
    for i in range(len(_ts_arr)):
        _latest[_pidx_arr[i]] = _mid_arr[i]
        _valid = _latest[~np.isnan(_latest)]
        if len(_valid) >= 3:
            _con_ts_list.append(_ts_arr[i])
            _con_px_list.append(np.median(_valid))

    con_ts = np.array(_con_ts_list)
    con_px = np.array(_con_px_list)
    print(f"  Consensus oracle: {len(con_ts)} prices from {_n_pools} pools")

    def consensus_at(ts_arr):
        idxs = np.searchsorted(con_ts, ts_arr, side="right") - 1
        return con_px[np.clip(idxs, 0, len(con_px) - 1)]

    proto_data = {}

    MIN_SWAP_USD = 1.0

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue
        swaps = detect_swaps(states)
        if swaps.height < 10:
            continue

        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        bd = swaps["bd"].to_numpy().astype(np.float64)
        qd = swaps["qd"].to_numpy().astype(np.float64)

        size_usd = np.abs(qd) / 1e6
        valid = bd != 0
        exec_price = np.full(len(bd), np.nan)
        exec_price[valid] = np.abs(qd[valid] / bd[valid]) * 1e3
        cmid = consensus_at(swap_ts)

        slip_bps = np.full(len(bd), np.nan)
        for i in range(len(bd)):
            if not valid[i] or cmid[i] == 0:
                continue
            if bd[i] > 0:
                slip_bps[i] = (cmid[i] - exec_price[i]) / cmid[i] * 10000
            else:
                slip_bps[i] = (exec_price[i] - cmid[i]) / cmid[i] * 10000

        ok = np.isfinite(slip_bps) & (size_usd >= MIN_SWAP_USD)
        if ok.sum() < 5:
            continue

        if amm not in proto_data:
            proto_data[amm] = ([], [])
        proto_data[amm][0].append(size_usd[ok])
        proto_data[amm][1].append(slip_bps[ok])

    if not proto_data:
        print("  No data for slippage scatter")
        return

    for amm in proto_data:
        proto_data[amm] = (
            np.concatenate(proto_data[amm][0]),
            np.concatenate(proto_data[amm][1]),
        )

    _proto_order = [
        "humidifi",
        "bisonfi",
        "bison_fi",
        "goonfi",
        "goon_fi",
        "solfiv2",
        "sol_fi_v2",
        "zerofi",
        "zero_fi",
    ]
    amms_sorted = [a for a in _proto_order if a in proto_data]
    amms_sorted += [a for a in proto_data if a not in amms_sorted]

    for amm in amms_sorted:
        sz, sl = proto_data[amm]
        print(
            f"  {PROTOCOL_LABELS.get(amm, amm):12s}: {len(sz):>7,} swaps (>=${MIN_SWAP_USD:.0f}), "
            f"median slip {np.median(sl):+.2f} bps, "
            f"p75 {np.percentile(sl, 75):+.2f} bps"
        )

    def bin_stats(sizes, slips, n_bins=12, min_count=60):
        log_s = np.log10(sizes)
        lo, hi = np.percentile(log_s, [1, 99])
        edges = np.linspace(lo, hi, n_bins + 1)
        centres, medians, p25s, p75s = [], [], [], []
        for j in range(n_bins):
            mask = (log_s >= edges[j]) & (log_s < edges[j + 1])
            if mask.sum() < min_count:
                continue
            sl = slips[mask]
            centres.append(10 ** ((edges[j] + edges[j + 1]) / 2))
            medians.append(np.median(sl))
            p25s.append(np.percentile(sl, 25))
            p75s.append(np.percentile(sl, 75))
        return (np.array(centres), np.array(medians), np.array(p25s), np.array(p75s))

    n_protos = len(amms_sorted)
    fig = plt.figure(figsize=(5.5, 4.3))

    gs = fig.add_gridspec(
        2,
        n_protos,
        height_ratios=[1, 1.3],
        hspace=0.38,
        wspace=0.05,
        left=0.10,
        right=0.98,
        top=0.93,
        bottom=0.11,
    )

    x_lo, x_hi = 0.8, 8e3
    y_lo_facet, y_hi_facet = -4, 8

    for col_i, amm in enumerate(amms_sorted):
        ax = fig.add_subplot(gs[0, col_i])
        sz, sl = proto_data[amm]
        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)

        n_pts = len(sz)
        if n_pts > 2500:
            idx = np.random.default_rng(42).choice(n_pts, 2500, replace=False)
            sz_s, sl_s = sz[idx], sl[idx]
        else:
            sz_s, sl_s = sz, sl

        sl_clip = np.clip(sl_s, y_lo_facet, y_hi_facet)
        ax.scatter(sz_s, sl_clip, s=0.3, alpha=0.10, color=color, rasterized=True, zorder=1)

        centres, medians, p25, p75 = bin_stats(sz, sl)
        if len(centres) >= 3:
            ax.plot(centres, medians, color=color, linewidth=1.3, zorder=5)

        ax.axhline(0, color="black", linewidth=0.3, linestyle=":", zorder=2)
        ax.set_xscale("log")
        ax.set_xlim(x_lo, x_hi)
        ax.set_ylim(y_lo_facet, y_hi_facet)
        ax.set_title(label, fontsize=6.5, pad=2, fontweight="semibold")
        ax.tick_params(labelsize=5, which="both")
        ax.grid(True, alpha=0.15, linewidth=0.3)

        ax.text(
            0.95,
            0.05,
            f"n={n_pts:,}",
            transform=ax.transAxes,
            fontsize=4.5,
            ha="right",
            va="bottom",
            color="0.5",
        )

        if col_i == 0:
            ax.set_ylabel("Slip. (bps)", fontsize=6.5)
        else:
            ax.set_yticklabels([])

        ax.set_xticks([10, 1000])
        ax.set_xticklabels(["$10", "$1k"], fontsize=4.5)
        ax.xaxis.set_minor_locator(LogLocator(base=10, subs="auto", numticks=10))
        ax.xaxis.set_minor_formatter(NullFormatter())

    ax_cmp = fig.add_subplot(gs[1, :])

    markers = ["o", "s", "D", "^", "v"]
    for i, amm in enumerate(amms_sorted):
        sz, sl = proto_data[amm]
        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)
        centres, medians, p25, p75 = bin_stats(sz, sl)
        if len(centres) < 3:
            continue

        mk = markers[i % len(markers)]
        ax_cmp.plot(
            centres,
            medians,
            color=color,
            linewidth=1.7,
            label=label,
            marker=mk,
            markersize=3.5,
            zorder=5,
            markeredgewidth=0.4,
            markeredgecolor="white",
        )

    ax_cmp.set_xscale("log")
    ax_cmp.set_xlabel("Swap size (USD)", fontsize=8)
    ax_cmp.set_ylabel("Median slippage vs consensus mid (bps)", fontsize=7.5)
    ax_cmp.axhline(0, color="black", linewidth=0.3, linestyle=":")
    ax_cmp.set_xlim(x_lo, x_hi)
    ax_cmp.set_ylim(-0.8, 2.5)
    ax_cmp.tick_params(labelsize=7)
    ax_cmp.grid(True, alpha=0.2, linewidth=0.4)

    ax_cmp.set_xticks([1, 10, 100, 1000])
    ax_cmp.set_xticklabels(["$1", "$10", "$100", "$1k"], fontsize=7)
    ax_cmp.xaxis.set_minor_formatter(NullFormatter())

    ax_cmp.legend(
        fontsize=6.5,
        loc="upper right",
        framealpha=0.92,
        edgecolor="0.8",
        handlelength=1.8,
        ncol=1,
        columnspacing=0.8,
        labelspacing=0.3,
    )

    path_out = os.path.join(out_dir, "plot_slippage_scatter.pdf")
    fig.savefig(path_out, dpi=200)
    plt.close(fig)
    print(f"Saved: {path_out}")


def section_markout(session_dir: str, metadata: dict, pyth_sol: PythIndex, out_dir: str):
    print(f"\n{SEP}\nSECTION: Multi-Horizon Markout PnL Analysis\n{SEP}")

    HORIZONS_S = [1, 2, 5, 10, 30, 60, 120, 300, 600]
    HORIZONS_MS = [h * 1000 for h in HORIZONS_S]
    HORIZON_LABELS = [f"{h}s" for h in HORIZONS_S]

    import glob as _glob

    _q_files = sorted(
        _glob.glob(os.path.join(session_dir, "quoter_output", "SOL-USDC", "quotes_*.parquet"))
    )
    if not _q_files:
        print("  No quote files found, cannot build consensus oracle")
        return
    _quotes = pl.concat([pl.read_parquet(f) for f in _q_files])
    _quotes = _quotes.filter(pl.col("slot") > 0).filter(pl.col("output_amount") > 0)
    _PS = PRICE_SCALE

    _b2q = (
        _quotes
        .filter((pl.col("direction") == "B2Q") & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1))
        .with_columns(
            (
                pl.col("output_amount").cast(pl.Float64)
                / pl.col("input_amount").cast(pl.Float64)
                * _PS
            ).alias("bid")
        )
        .select(["timestamp_ms", "slot", "pool_id", "bid"])
    )
    _q2b = (
        _quotes
        .filter((pl.col("direction") == "Q2B") & ((pl.col("input_usd_equiv") - 100.0).abs() < 0.1))
        .with_columns(
            (
                pl.col("input_amount").cast(pl.Float64)
                / pl.col("output_amount").cast(pl.Float64)
                * _PS
            ).alias("ask")
        )
        .select(["slot", "pool_id", "ask"])
    )
    _mids = (
        _b2q
        .join(_q2b, on=["slot", "pool_id"], how="inner")
        .with_columns(((pl.col("bid") + pl.col("ask")) / 2).alias("mid"))
        .select(["timestamp_ms", "slot", "pool_id", "mid"])
        .sort("timestamp_ms")
    )

    pool_id_to_amm = {}
    for pool in metadata["pools"]:
        pool_id_to_amm[pool["pool_id"]] = pool["amm_type"]

    pool_prefix_to_amm = {}
    for pool in metadata["pools"]:
        parts = pool["data_dir"].rsplit("_", 1)
        if len(parts) == 2:
            pool_prefix_to_amm[parts[1]] = pool["amm_type"]

    def get_amm_for_pool_id(pid):
        if pid in pool_id_to_amm:
            return pool_id_to_amm[pid]
        for prefix, amm in pool_prefix_to_amm.items():
            if pid.startswith(prefix):
                return amm
        return None

    _pool_ids = _mids["pool_id"].unique().sort().to_list()
    _pool_map = {pid: i for i, pid in enumerate(_pool_ids)}
    _n_pools = len(_pool_ids)

    pool_idx_to_amm = {}
    for pid in _pool_ids:
        amm = get_amm_for_pool_id(pid)
        if amm:
            pool_idx_to_amm[_pool_map[pid]] = amm

    _ts_arr = _mids["timestamp_ms"].to_numpy().astype(np.float64)
    _mid_arr = _mids["mid"].to_numpy()
    _pidx_arr = np.array([_pool_map[p] for p in _mids["pool_id"].to_list()])

    pool_ts_arrays = {}
    pool_mid_arrays = {}
    for pid_idx in range(_n_pools):
        mask = _pidx_arr == pid_idx
        if mask.sum() > 0:
            pool_ts_arrays[pid_idx] = _ts_arr[mask].copy()
            pool_mid_arrays[pid_idx] = _mid_arr[mask].copy()

    print(f"  Built per-pool mid arrays for {_n_pools} pools")

    def loo_consensus_vec(ts_arr_in, exclude_amm):
        n = len(ts_arr_in)
        result = np.full(n, np.nan)

        pool_indices = {}
        for pid_idx in range(_n_pools):
            amm = pool_idx_to_amm.get(pid_idx)
            if amm == exclude_amm:
                continue
            if pid_idx not in pool_ts_arrays:
                continue
            pts = pool_ts_arrays[pid_idx]
            idxs = np.searchsorted(pts, ts_arr_in, side="right") - 1
            pool_indices[pid_idx] = idxs

        included_pools = list(pool_indices.keys())
        n_included = len(included_pools)

        if n_included < 2:
            return result

        mid_matrix = np.full((n_included, n), np.nan)
        for row, pid_idx in enumerate(included_pools):
            idxs = pool_indices[pid_idx]
            valid = idxs >= 0
            mid_matrix[row, valid] = pool_mid_arrays[pid_idx][idxs[valid]]

        with warnings.catch_warnings():
            warnings.simplefilter("ignore", RuntimeWarning)
            result = np.nanmedian(mid_matrix, axis=0)

        n_valid = np.sum(~np.isnan(mid_matrix), axis=0)
        result[n_valid < 2] = np.nan

        return result

    proto_swaps = {}

    for pool in metadata["pools"]:
        amm = pool["amm_type"]
        if amm in SKIP_SWAP_PROTOCOLS or amm in EXCLUDE_CROSS_COMPARE:
            continue
        symbol = pool.get("symbol", "") or pool["data_dir"]
        if "SOL" not in symbol.upper() or "USDC" not in symbol.upper():
            continue

        states = load_pool_data(session_dir, pool["data_dir"], "derived_state")
        if states.height == 0:
            continue

        swaps = detect_swaps(states)
        if swaps.height < 5:
            continue

        swap_ts = swaps["timestamp_ms"].to_numpy().astype(np.float64)
        bd = swaps["bd"].to_numpy().astype(np.float64)
        qd = swaps["qd"].to_numpy().astype(np.float64)

        valid = bd != 0
        exec_price = np.full(len(bd), np.nan)
        exec_price[valid] = np.abs(qd[valid] / bd[valid]) * PRICE_SCALE

        d = np.sign(bd).astype(np.float64)
        vol_usd = np.abs(qd) / 1e6

        ok = np.isfinite(exec_price) & (vol_usd > 0.5)
        if ok.sum() < 5:
            continue

        if amm not in proto_swaps:
            proto_swaps[amm] = {"ts": [], "exec_price": [], "d": [], "vol_usd": []}
        proto_swaps[amm]["ts"].append(swap_ts[ok])
        proto_swaps[amm]["exec_price"].append(exec_price[ok])
        proto_swaps[amm]["d"].append(d[ok])
        proto_swaps[amm]["vol_usd"].append(vol_usd[ok])

    for amm in proto_swaps:
        proto_swaps[amm] = {
            "ts": np.concatenate(proto_swaps[amm]["ts"]),
            "exec_price": np.concatenate(proto_swaps[amm]["exec_price"]),
            "d": np.concatenate(proto_swaps[amm]["d"]),
            "vol_usd": np.concatenate(proto_swaps[amm]["vol_usd"]),
        }

    if not proto_swaps:
        print("  No swap data for markout analysis")
        return

    for amm in sorted(proto_swaps.keys()):
        n = len(proto_swaps[amm]["ts"])
        print(f"  {PROTOCOL_LABELS.get(amm, amm)}: {n} swaps")

    print("\n  Computing markouts (LOO consensus at multiple horizons)...")

    proto_markouts = {}

    for amm in sorted(proto_swaps.keys()):
        data = proto_swaps[amm]
        n_swaps = len(data["ts"])
        ts = data["ts"]
        exec_p = data["exec_price"]
        d = data["d"]

        print(f"  {PROTOCOL_LABELS.get(amm, amm)} ({n_swaps} swaps)...", end=" ", flush=True)

        M_t = loo_consensus_vec(ts, amm)

        markouts = np.full((len(HORIZONS_MS), n_swaps), np.nan)
        for h_idx, h_ms in enumerate(HORIZONS_MS):
            M_t_tau = loo_consensus_vec(ts + h_ms, amm)
            valid = np.isfinite(M_t) & np.isfinite(M_t_tau) & (M_t > 0)
            markouts[h_idx, valid] = (
                d[valid] * (M_t_tau[valid] - exec_p[valid]) / M_t[valid] * 10000
            )

        proto_markouts[amm] = markouts
        print(f"done (5s mean: {np.nanmean(markouts[2]):.2f} bps)")

    print(f"\n{SEP}")
    print("MARKOUT RESULTS (bps, positive = LP profit, negative = adverse selection)")
    print(SEP)

    header = f"{'Protocol':<15s}" + "".join(f"  {h:>6s}" for h in HORIZON_LABELS)
    print("\n  Equal-weighted mean markout:")
    print(f"  {header}")
    for amm in sorted(proto_markouts.keys()):
        label = PROTOCOL_LABELS.get(amm, amm)
        vals = "".join(
            f"  {np.nanmean(proto_markouts[amm][h]):>6.2f}" for h in range(len(HORIZONS_MS))
        )
        print(f"  {label:<15s}{vals}")

    print("\n  Volume-weighted mean markout:")
    print(f"  {header}")
    for amm in sorted(proto_markouts.keys()):
        label = PROTOCOL_LABELS.get(amm, amm)
        vol = proto_swaps[amm]["vol_usd"]
        vals = ""
        for h in range(len(HORIZONS_MS)):
            m = proto_markouts[amm][h]
            finite = np.isfinite(m)
            if finite.sum() > 0:
                wm = np.average(m[finite], weights=vol[finite])
                vals += f"  {wm:>6.2f}"
            else:
                vals += f"  {'N/A':>6s}"
        print(f"  {label:<15s}{vals}")

    print("\n  Breakeven horizon (tau where mean markout crosses zero):")
    for amm in sorted(proto_markouts.keys()):
        label = PROTOCOL_LABELS.get(amm, amm)
        means = [np.nanmean(proto_markouts[amm][h]) for h in range(len(HORIZONS_MS))]
        breakeven = None
        for h in range(len(means) - 1):
            if np.isfinite(means[h]) and np.isfinite(means[h + 1]):
                if means[h] * means[h + 1] < 0:
                    t0_h = HORIZONS_S[h]
                    t1_h = HORIZONS_S[h + 1]
                    frac = abs(means[h]) / (abs(means[h]) + abs(means[h + 1]))
                    breakeven = t0_h + frac * (t1_h - t0_h)
                    break
        if breakeven is not None:
            print(f"  {label:<15s}: ~{breakeven:.1f}s")
        elif all(m > 0 for m in means if np.isfinite(m)):
            print(f"  {label:<15s}: always positive (LP profitable at all horizons)")
        elif all(m < 0 for m in means if np.isfinite(m)):
            print(f"  {label:<15s}: always negative (persistent adverse selection)")
        else:
            print(f"  {label:<15s}: indeterminate")

    print("\n  95% Bootstrap CI for mean markout:")
    n_boot = 5000
    rng = np.random.default_rng(42)
    for h_idx, h_label in [(2, "5s"), (5, "60s")]:
        print(f"\n  Horizon {h_label}:")
        for amm in sorted(proto_markouts.keys()):
            label = PROTOCOL_LABELS.get(amm, amm)
            m = proto_markouts[amm][h_idx]
            finite = np.isfinite(m)
            vals = m[finite]
            if len(vals) < 10:
                print(f"    {label:<15s}: insufficient data")
                continue
            boot_means = np.empty(n_boot)
            for b in range(n_boot):
                sample = rng.choice(vals, size=len(vals), replace=True)
                boot_means[b] = np.mean(sample)
            ci_lo, ci_hi = np.percentile(boot_means, [2.5, 97.5])
            print(
                f"    {label:<15s}: mean={np.mean(vals):>6.2f} bps, 95% CI [{ci_lo:>6.2f}, {ci_hi:>6.2f}]"
            )

    _proto_order = ["humidifi", "bisonfi", "goonfi", "solfiv2", "zerofi"]
    amms_sorted = [a for a in _proto_order if a in proto_markouts]
    amms_sorted += [a for a in sorted(proto_markouts.keys()) if a not in amms_sorted]

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(10, 4), sharey=True)

    for amm in amms_sorted:
        color = PROTOCOL_COLORS.get(amm, "gray")
        label = PROTOCOL_LABELS.get(amm, amm)
        vol = proto_swaps[amm]["vol_usd"]

        ew_means = []
        ew_ci_lo = []
        ew_ci_hi = []
        vw_means = []
        for h in range(len(HORIZONS_MS)):
            m = proto_markouts[amm][h]
            finite = np.isfinite(m)
            vals_h = m[finite]
            if len(vals_h) > 0:
                mu = np.mean(vals_h)
                se = np.std(vals_h) / np.sqrt(len(vals_h))
                ew_means.append(mu)
                ew_ci_lo.append(mu - 1.96 * se)
                ew_ci_hi.append(mu + 1.96 * se)
                vw_means.append(np.average(vals_h, weights=vol[finite]))
            else:
                ew_means.append(np.nan)
                ew_ci_lo.append(np.nan)
                ew_ci_hi.append(np.nan)
                vw_means.append(np.nan)

        ew_means = np.array(ew_means)
        ew_ci_lo = np.array(ew_ci_lo)
        ew_ci_hi = np.array(ew_ci_hi)
        vw_means = np.array(vw_means)

        ax1.plot(
            HORIZONS_S, ew_means, color=color, marker="o", markersize=3, linewidth=1.5, label=label
        )
        ax1.fill_between(HORIZONS_S, ew_ci_lo, ew_ci_hi, color=color, alpha=0.1)
        ax2.plot(
            HORIZONS_S, vw_means, color=color, marker="o", markersize=3, linewidth=1.5, label=label
        )

    for ax, title in [(ax1, "Equal-Weighted"), (ax2, "Volume-Weighted")]:
        ax.set_xscale("log")
        ax.set_xlabel("Horizon (seconds)")
        ax.set_title(title)
        ax.axhline(0, color="black", linewidth=0.5, linestyle="--")
        ax.grid(True, alpha=0.3, which="both")
        ax.legend(fontsize=7, loc="best")
        ax.set_xticks(HORIZONS_S)
        ax.set_xticklabels([str(h) for h in HORIZONS_S], fontsize=7)

    ax1.set_ylabel("Mean Markout (bps)")
    fig.suptitle("LP Markout by Horizon (positive = LP profit)", fontsize=11)
    fig.tight_layout()
    path1 = os.path.join(out_dir, "emp_markout_curves.pdf")
    fig.savefig(path1)
    plt.close(fig)
    print(f"\n  Saved: {path1}")

    fig2, (ax3, ax4) = plt.subplots(1, 2, figsize=(10, 4.5))

    for ax, h_idx, h_label in [(ax3, 2, "5s"), (ax4, 5, "60s")]:
        n_quintiles = 5
        width = 0.8 / max(len(amms_sorted), 1)

        for i, amm in enumerate(amms_sorted):
            m = proto_markouts[amm][h_idx]
            vol = proto_swaps[amm]["vol_usd"]
            finite = np.isfinite(m)
            if finite.sum() < 20:
                continue

            m_f = m[finite]
            v_f = vol[finite]

            quintile_edges = np.percentile(v_f, np.linspace(0, 100, n_quintiles + 1))
            q_means = []
            for q in range(n_quintiles):
                lo = quintile_edges[q]
                hi = quintile_edges[q + 1]
                if q == n_quintiles - 1:
                    mask_q = (v_f >= lo) & (v_f <= hi)
                else:
                    mask_q = (v_f >= lo) & (v_f < hi)
                if mask_q.sum() > 0:
                    q_means.append(np.mean(m_f[mask_q]))
                else:
                    q_means.append(np.nan)

            x = np.arange(n_quintiles)
            offset = (i - len(amms_sorted) / 2 + 0.5) * width
            color = PROTOCOL_COLORS.get(amm, "gray")
            label_name = PROTOCOL_LABELS.get(amm, amm)
            ax.bar(x + offset, q_means, width, color=color, label=label_name, alpha=0.8)

        ax.axhline(0, color="black", linewidth=0.5, linestyle="--")
        ax.set_xlabel("Trade Size Quintile")
        ax.set_ylabel("Mean Markout (bps)")
        ax.set_title(f"Markout at {h_label} Horizon by Size")
        ax.set_xticks(range(n_quintiles))
        ax.set_xticklabels([f"Q{q + 1}" for q in range(n_quintiles)])
        ax.legend(fontsize=6, loc="best")
        ax.grid(True, alpha=0.3, axis="y")

    fig2.suptitle("Markout by Trade Size Quintile", fontsize=11)
    fig2.tight_layout()
    path2 = os.path.join(out_dir, "emp_markout_size.pdf")
    fig2.savefig(path2)
    plt.close(fig2)
    print(f"  Saved: {path2}")

    print("\n  Markout analysis complete.")


SECTIONS = {
    "overview": lambda sd, m, ps, pd, od: section_overview(sd, m, ps, od),
    "spreads": lambda sd, m, ps, pd, od: section_spreads(sd, m, ps, pd, od),
    "risk": lambda sd, m, ps, pd, od: section_risk(sd, m, ps, od),
    "competition": lambda sd, m, ps, pd, od: section_competition(sd, m, ps, od),
    "execution": lambda sd, m, ps, pd, od: section_execution(sd, m, ps, od),
    "microstructure": lambda sd, m, ps, pd, od: section_microstructure(sd, m, ps, od),
    "jitter": lambda sd, m, ps, pd, od: section_jitter(sd, m, ps, od),
    "markout": lambda sd, m, ps, pd, od: section_markout(sd, m, ps, od),
}


def main():
    parser = argparse.ArgumentParser(description="pAMM empirical analysis (Chapter 7)")
    parser.add_argument("session_dir", help="Path to session directory")
    parser.add_argument(
        "--section",
        default="all",
        choices=["all"] + list(SECTIONS.keys()),
        help="Which section to run (default: all)",
    )
    args = parser.parse_args()

    session_dir = args.session_dir
    out_dir = os.path.join(session_dir, "analysis")
    os.makedirs(out_dir, exist_ok=True)

    print(f"{SEP}\npAMM Empirical Analysis\nSession: {session_dir}\nOutput:  {out_dir}\n{SEP}")

    t0 = time.time()

    metadata = load_metadata(session_dir)
    print(f"Loaded metadata: {len(metadata['pools'])} pools")

    print("Loading Pyth prices...")
    pyth_df = load_pyth(session_dir)
    pyth_sol = PythIndex(pyth_df, "SOL/USD")
    print(f"  SOL/USD prices: {len(pyth_sol.ts)}")

    sections_to_run = list(SECTIONS.keys()) if args.section == "all" else [args.section]

    for name in sections_to_run:
        t1 = time.time()
        SECTIONS[name](session_dir, metadata, pyth_sol, pyth_df, out_dir)
        print(f"  [{name} done in {time.time() - t1:.1f}s]")

    total = time.time() - t0
    print(f"\n{SEP}\nALL ANALYSES COMPLETE\nTotal time: {total:.1f}s\nOutput: {out_dir}\n{SEP}")

    for f in sorted(os.listdir(out_dir)):
        size_kb = os.path.getsize(os.path.join(out_dir, f)) / 1024
        print(f"  {f} ({size_kb:.1f} KB)")


if __name__ == "__main__":
    main()
