// A mock OAuth provider in the shape of pi's Radius flow (never a real provider).
//
//   node mock-oauth.mjs LOG
//
// Prints its port on stdout. Endpoints:
// - POST /v1/oauth/token: `refresh_token` grants return access-<n>/refresh-<n>, valid for an
//   hour ("rt-revoked" gets a 400 invalid_grant); `authorization_code` grants accept the code
//   "good"; device-code grants are pending once, then succeed. Each request appends
//   "<grant_type> <refresh_token|code|device_code>" to LOG.
// - GET /v1/oauth: the authorization endpoint (never visited: tests paste the redirect URL).
// - POST /v1/oauth/device: a device code.
import { appendFileSync } from "node:fs";
import http from "node:http";

const log = process.argv[2];
let issued = 0;
const polled = new Set();

function tokens(res) {
  issued += 1;
  res.end(JSON.stringify({ access_token: `access-${issued}`, refresh_token: `refresh-${issued}`, expires_in: 3600 }));
}

function error(res, status, code, description) {
  res.statusCode = status;
  res.end(JSON.stringify({ error: code, error_description: description }));
}

const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (chunk) => (body += chunk));
  req.on("end", () => {
    res.setHeader("content-type", "application/json");
    const port = server.address().port;
    if (req.method === "GET" && req.url === "/v1/oauth") {
      res.end(JSON.stringify({ authorizationEndpoint: `http://127.0.0.1:${port}/authorize` }));
      return;
    }
    if (req.method === "POST" && req.url === "/v1/oauth/device") {
      res.end(
        JSON.stringify({
          device_code: "dc-1",
          user_code: "ABCD-1234",
          verification_uri: "https://id.example.com/device",
          expires_in: 60,
          interval: 1,
        }),
      );
      return;
    }
    if (req.method !== "POST" || req.url !== "/v1/oauth/token") {
      res.statusCode = 404;
      res.end("{}");
      return;
    }
    const form = new URLSearchParams(body);
    const grant = form.get("grant_type");
    const subject = form.get("refresh_token") ?? form.get("code") ?? form.get("device_code");
    appendFileSync(log, `${grant} ${subject}\n`);
    if (grant === "refresh_token") {
      if (subject === "rt-revoked") error(res, 400, "invalid_grant", "the refresh token was revoked");
      else tokens(res);
    } else if (grant === "authorization_code") {
      if (subject === "good") tokens(res);
      else error(res, 400, "invalid_grant", "the code is not valid");
    } else if (polled.has(subject)) {
      tokens(res);
    } else {
      polled.add(subject);
      error(res, 400, "authorization_pending", "not yet");
    }
  });
});
server.listen(0, "127.0.0.1", () => console.log(server.address().port));
