FROM python:3.12-slim

WORKDIR /app
COPY pyproject.toml ./
COPY jev_decider ./jev_decider
RUN pip install --no-cache-dir .

ENV HOST=0.0.0.0 PORT=8080
EXPOSE 8080
CMD ["hiroute-jev-decider"]
