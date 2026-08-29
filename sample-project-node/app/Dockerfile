# Fully-qualified (docker.io/library/...) rather than a short name: rootless
# Podman doesn't ship with unqualified-search-registries configured by
# default, so "node:20-alpine" fails to resolve on a fresh install.
FROM docker.io/library/node:20-alpine
WORKDIR /app

# Cache dependencies in their own layer before copying source.
COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY index.js ./

EXPOSE 8080
CMD ["node", "index.js"]
