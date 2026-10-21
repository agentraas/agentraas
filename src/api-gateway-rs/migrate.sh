#!/bin/sh
# Applies every migration in /migrations, in order, under an advisory lock so
# replicas starting together don't race. Every migration is safe to re-run.
# Run it before the API starts on a platform without compose or Helm:
#   ECS init container, Cloud Run job, Container Apps job: `agentraas-migrate`.
set -e
until pg_isready -q -d "$DATABASE_URL"; do echo "waiting for postgres"; sleep 2; done
{ echo "SELECT pg_advisory_lock(727274);"; for f in /migrations/*.sql; do echo "\\i $f"; done; } \
  | psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q
echo "migrations applied"
