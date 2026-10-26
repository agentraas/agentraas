import os, requests
from agentraas.local import exactly_once
API = os.environ["AGENTRAAS_CHAOS_URL"]

@exactly_once()
def charge(customer, amount, idempotency_key=None):
    for attempt in range(3):
        try:
            return requests.post(f"{API}/v1/charges",
                                 data={"customer": customer, "amount": amount},
                                 headers={"Idempotency-Key": idempotency_key}, timeout=5).json()
        except requests.RequestException:
            continue

charge("cus_42", 4200)
