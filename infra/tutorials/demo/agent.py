import os, requests
API = os.environ["AGENTRAAS_CHAOS_URL"]

def charge(customer, amount):
    for attempt in range(3):          # retry on network errors
        try:
            return requests.post(f"{API}/v1/charges",
                                 data={"customer": customer, "amount": amount}, timeout=5).json()
        except requests.RequestException:
            continue

charge("cus_42", 4200)
