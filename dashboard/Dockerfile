# ---- build the UI ----
# node 24 = current LTS with npm 11: npm 10 (node 20/22) false-positives on
# this lockfile's optional peer deps and refuses `npm ci`.
FROM node:24-slim AS ui
WORKDIR /ui
COPY dashboard/frontend/package.json dashboard/frontend/package-lock.json ./
RUN npm ci
COPY dashboard/frontend/ ./
RUN npm run build

# ---- runtime: FastAPI serving the API + the built UI on one port ----
FROM python:3.12-slim
RUN useradd -mu 10001 dash
WORKDIR /app/dashboard/backend
COPY dashboard/backend/requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt
COPY dashboard/backend/ ./
# The backend reads the project version from Cargo.toml so the collector and the
# dashboard can never report different versions. Without this the container has no
# Cargo.toml and the dashboard shows "unknown".
COPY Cargo.toml /app/Cargo.toml
COPY --from=ui /ui/dist /app/dashboard/frontend/dist
RUN chown -R dash /app
# server.py serves dist/ (SPA) alongside /api when SERVE_STATIC=1.
ENV SERVE_STATIC=1
EXPOSE 8000
# Drop privileges (matches the collector image). The Config tab edits
# config.toml and "Restart to apply" writes a sentinel under /data/control —
# both are rw bind mounts, so their host paths must be writable by uid 10001.
USER dash
CMD ["python", "-m", "uvicorn", "server:app", "--host", "0.0.0.0", "--port", "8000"]
