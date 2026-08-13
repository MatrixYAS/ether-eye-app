import sqlite3

c = sqlite3.connect("/tmp/ether_engine_smoke/ether_engine.db")
for r in c.execute(
    "SELECT id, status, round(net_profit_usd,2), round(size_usd,1),"
    " round(optimal_size_usd,1), verification_status, route, rejection_reason"
    " FROM opportunities"
):
    print(r)
print("---legs---")
for r in c.execute(
    "SELECT opportunity_id, leg_index, venue, amount_in, amount_out,"
    " round(price_impact_bps,2), liquidity_ok FROM opportunity_legs ORDER BY 1,2"
):
    print(r)
