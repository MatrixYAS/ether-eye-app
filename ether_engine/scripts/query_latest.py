import sqlite3

c = sqlite3.connect("/tmp/ether_engine_smoke/ether_engine.db")
last = c.execute("SELECT MAX(id) FROM opportunities").fetchone()[0]
first = last - 23  # last full batch of 24 routes
print("=== latest batch ids", first + 1, "-", last)
for r in c.execute(
    "SELECT id, status, round(net_profit_usd,2), round(size_usd,1),"
    " round(optimal_size_usd,1), verification_status, route, rejection_reason"
    " FROM opportunities WHERE id >= ? ORDER BY id",
    (first,),
):
    print(r)
for lid in c.execute("SELECT id FROM opportunities WHERE id >= ? ORDER BY id LIMIT 1", (first,)).fetchone():
    for leg in c.execute(
        "SELECT leg_index, venue, amount_in, amount_out, round(price_impact_bps,1)"
        " FROM opportunity_legs WHERE opportunity_id = ?",
        (lid,),
    ):
        print("  leg:", leg)
