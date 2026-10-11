#!/usr/bin/env node
// Prints a license token signed with the repo's TEST key, for a server built
// with --features test-license-key (CI's self-hosted run). A release build
// rejects it. Usage: node test/mint-test-license.js [team|enterprise] [ttl_seconds]
const crypto = require('crypto');
const path = require('path');
const fs = require('fs');

const key = fs.readFileSync(path.join(__dirname, '../crates/core/test_license_private_key.pem'), 'utf8');
const b64 = (obj) => Buffer.from(JSON.stringify(obj)).toString('base64url');
const now = Math.floor(Date.now() / 1000);
const unsigned = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64({ sub: 'ci', tier: process.argv[2] || 'enterprise', iat: now, exp: now + Number(process.argv[3] || 86400) })}`;
const sig = crypto.sign('RSA-SHA256', Buffer.from(unsigned), key).toString('base64url');
process.stdout.write(`${unsigned}.${sig}\n`);
