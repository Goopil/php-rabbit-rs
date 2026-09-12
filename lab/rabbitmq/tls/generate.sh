#!/usr/bin/env bash
# Generates the TLS lab certificates into ./generated (gitignored):
#
#   lab-ca.pem / lab-ca.key        trusted lab root CA (signs the server cert)
#   lab-other-ca.pem / ...key      deliberately untrusted CA (negative tests)
#   server.pem / server.key        broker certificate signed by the lab CA
#   lab-client.pem / ...key        mTLS client identity signed by the lab CA
#
# Server certificate names (SANs): rabbit.internal, localhost, 127.0.0.1
# The TLS nodes mount ./generated at /etc/rabbitmq/tls (see compose.yaml,
# rabbitmq-tls.conf and rabbitmq-mtls.conf).
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${DIR}/generated"
DAYS=3650

command -v openssl >/dev/null 2>&1 || { echo "ERROR: openssl is required" >&2; exit 1; }

mkdir -p "${OUT}"
rm -f "${OUT}"/lab-ca.pem "${OUT}"/lab-ca.key \
    "${OUT}"/lab-other-ca.pem "${OUT}"/lab-other-ca.key \
    "${OUT}"/lab-client.pem "${OUT}"/lab-client.key "${OUT}"/lab-client.csr \
    "${OUT}"/server.pem "${OUT}"/server.key "${OUT}"/server.csr

# 1. Trusted lab root CA.
openssl req -x509 -newkey rsa:2048 -nodes -days "${DAYS}" \
    -keyout "${OUT}/lab-ca.key" -out "${OUT}/lab-ca.pem" \
    -subj "/CN=Rabbit RS Lab Root CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1

# 2. Untrusted CA: same shape, different key and subject, never used to sign
#    the server certificate. Handing this file as tls.ca_cert must fail the
#    handshake.
openssl req -x509 -newkey rsa:2048 -nodes -days "${DAYS}" \
    -keyout "${OUT}/lab-other-ca.key" -out "${OUT}/lab-other-ca.pem" \
    -subj "/CN=Rabbit RS Foreign CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1

# 3. Server certificate signed by the trusted lab CA.
openssl req -newkey rsa:2048 -nodes \
    -keyout "${OUT}/server.key" -out "${OUT}/server.csr" \
    -subj "/CN=rabbit.internal" >/dev/null 2>&1
openssl x509 -req -in "${OUT}/server.csr" \
    -CA "${OUT}/lab-ca.pem" -CAkey "${OUT}/lab-ca.key" -CAcreateserial \
    -days 825 \
    -extfile <(printf '%s\n' \
        "basicConstraints=CA:FALSE" \
        "keyUsage=digitalSignature,keyEncipherment" \
        "extendedKeyUsage=serverAuth" \
        "subjectAltName=DNS:rabbit.internal,DNS:localhost,IP:127.0.0.1") \
    -out "${OUT}/server.pem" >/dev/null 2>&1
rm -f "${OUT}/server.csr" "${OUT}/lab-ca.srl"

# 4. mTLS client identity signed by the trusted lab CA. No SAN required for
#    client certificates, but the clientAuth extended key usage is: RabbitMQ
#    rejects certificates without it when it verifies peer certificates.
openssl req -newkey rsa:2048 -nodes \
    -keyout "${OUT}/lab-client.key" -out "${OUT}/lab-client.csr" \
    -subj "/CN=rabbit-rs-lab-client" >/dev/null 2>&1
openssl x509 -req -in "${OUT}/lab-client.csr" \
    -CA "${OUT}/lab-ca.pem" -CAkey "${OUT}/lab-ca.key" -CAcreateserial \
    -days 825 \
    -extfile <(printf '%s\n' \
        "basicConstraints=CA:FALSE" \
        "keyUsage=digitalSignature,keyEncipherment" \
        "extendedKeyUsage=clientAuth") \
    -out "${OUT}/lab-client.pem" >/dev/null 2>&1
rm -f "${OUT}/lab-client.csr" "${OUT}/lab-ca.srl"

# lab-ca.key, lab-other-ca.key and lab-client.key are only read by the
# generating user (generation and the test process), so 0600 stays. The
# broker reads server.key as the container's `rabbitmq` user (uid 999); on
# Linux bind mounts honor the host uid, so 0600 owned by the runner makes
# the key unreadable and the TLS nodes crash-loop at startup. The key is a
# disposable lab secret regenerated on every lab-up (never committed), so
# 0644 inside the ephemeral lab is the accepted trade.
chmod 600 "${OUT}/lab-ca.key" "${OUT}/lab-other-ca.key" "${OUT}/lab-client.key"
chmod 644 "${OUT}/server.key"

echo "TLS lab certificates generated in ${OUT}:"
ls -1 "${OUT}" | sed 's/^/  /'
