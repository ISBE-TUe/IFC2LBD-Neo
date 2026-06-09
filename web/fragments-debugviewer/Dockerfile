FROM node:22-alpine

WORKDIR /app

COPY package.json package-lock.json ./
COPY scripts ./scripts
RUN npm ci

COPY . .

EXPOSE 3001

CMD ["npm", "run", "dev"]
