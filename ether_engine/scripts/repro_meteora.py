import math

# Stored leg B inputs (opportunity 398): amount_in = 6684072 lamports SOL,
# venue Meteora DLMM, active_bin 5068, bin_step_bps 10, liquidity_quote 2.6M,
# fee_num = bin_step_bps/2 = 5, fee_den = 10000.
amount_in = 6_684_072
dec_in = 9
dec_out = 6
active_bin = 5_068
bin_step_bps = 10
max_bins = 10
liquidity_quote = 2_600_000.0
base = 1 + bin_step_bps / 1e4
start_price = 150.70
fee_rate = (bin_step_bps / 2) / 1e4  # 5/10000 = 0.0005

amount_in_f = amount_in / 10 ** dec_in
amount_in_after = amount_in_f * (1 - fee_rate)
remaining = amount_in_after
produced = 0.0
end_price = start_price
bins = 0
for step in range(1, max_bins + 1):
    if remaining <= 0:
        break
    p = start_price * base ** step
    bin_usd = liquidity_quote / max_bins
    take_out = min(bin_usd, remaining * p)
    produced += take_out
    remaining -= take_out / p
    end_price = p
    bins += 1

amount_out = math.floor(produced * 10 ** dec_out)
ideal_out = amount_in_f * start_price
impact = (1 - produced / ideal_out) * 10_000
print("bins:", bins, "end_price:", round(end_price, 4))
print("produced:", produced, "amount_out:", amount_out)
print("ideal_out:", ideal_out, "impact_bps:", round(impact, 2))
print("expected stored: amount_out=1058060, impact=-504.03, bins=2, end=158.77")

# Also check what input would give amount_out=1058060 with 2 bins end=158.77
# Reverse: produced = 1.058060 USDC. impact -504 → produced/ideal = 1.0503
# → ideal = 1.00737 → amount_in_f = ideal/150.70 = 0.0066856 → in = 6685600 ≈ 6684072 ✓
# So produced=1.058 with 2 bins ending at 158.77??
# If remaining after bin1 stayed >0: bin1 take = min(260000, rem*p1) = rem*p1 always
# → produced += rem*p1; remaining -= rem (→0). Hmm unless take = bin_usd < rem*p.
# 260000 < rem*p requires rem > 260000/150.85 = 1723 SOL → $258k in.
# Not our case. So something else produced 1.058 with 2 bins...
# OLD code: take_out = binUSD.min(rem*p); produced += take_out; remaining -= take_out/p
# — same. What about sign? zero_for_one sign=+1 → offset=+step. What if old sign=-1:
p2 = start_price * base ** (-2)
print("price at offset -2:", p2)
