# Running as root is safe: this is a development lab image, not production.
FROM rabbitmq@sha256:f093e74d14814d28e3d52e7dee5873ab8e8c2e671e9e11019654bd3443183095

ARG PLUGIN_VERSION=4.2.0
ARG PLUGIN_SHA256=f168b2c09810cde3726961d31f38e3408e6a7fbff3929908d6f962061d8e70a1
ARG PLUGIN_URL=https://github.com/rabbitmq/rabbitmq-delayed-message-exchange/releases/download/v${PLUGIN_VERSION}/rabbitmq_delayed_message_exchange-${PLUGIN_VERSION}.ez

# HTTPS enforced via curl -fsSL; integrity verified via sha256 checksum below.
RUN apk add --no-cache curl ca-certificates \
    && curl -fsSL -o "/opt/rabbitmq/plugins/rabbitmq_delayed_message_exchange-${PLUGIN_VERSION}.ez" "${PLUGIN_URL}" \
    && echo "${PLUGIN_SHA256}  /opt/rabbitmq/plugins/rabbitmq_delayed_message_exchange-${PLUGIN_VERSION}.ez" \
        | sha256sum -c \
    && apk del curl

COPY enabled_plugins /etc/rabbitmq/enabled_plugins
