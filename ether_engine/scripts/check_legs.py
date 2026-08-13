import sqlite3

c = sqlite3.connect("/tmp/ether_engine_smoke/ether_engine.db")
for row in c.execute(
    "SELECT opportunity_id, leg_index, venue, token_in[-12:], token_out[-12:], "
    "amount_in, amount_out, fee_lamports, round(price_impact_bps, 2), "
    "round(price_impact_bps,2) * -1 AS neg, liquidity_ok, quote_slot "
    "FROM opportunity_legs WHERE opportunity_id IN (398) ORDER BY 1, 2"
):
    print(row)
print("---")
# also show all columns without truncation
for row in c.execute(
    "SELECT opportunity_id, leg_index, amount_in, amount_out FROM opportunity_legs WHERE opportunity_id=398"
):
    print("len", len(str(row[2])), len(str(row[3])), row)
