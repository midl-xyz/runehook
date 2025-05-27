FROM node:22-alpine AS base

FROM base AS deps
WORKDIR /app
COPY package*.json ./
RUN npm install

FROM base AS builder
WORKDIR /app
COPY . .
COPY --from=deps /app/node_modules ./node_modules
RUN npm run build

FROM base AS git-info
WORKDIR /app
ARG GIT_COMMIT
ARG GIT_BRANCH
COPY package.json ./
RUN echo "${GIT_BRANCH}" > .git-info && \
    echo "${GIT_COMMIT}" >> .git-info && \
    NPM_VERSION=$(node -p "require('./package.json').version") && \
    echo "v${NPM_VERSION}" >> .git-info 

FROM base AS runner
WORKDIR /app
COPY --from=builder /app/dist ./dist
COPY package*.json ./
COPY --from=deps /app/node_modules ./node_modules
COPY --from=git-info /app/.git-info ./
RUN npm prune --production
CMD ["node", "./dist/src/index.js"]