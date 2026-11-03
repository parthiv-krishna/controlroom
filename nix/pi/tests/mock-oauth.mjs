// A mock OAuth token endpoint (pi's Radius flow: POST /v1/oauth/token).
//
//   node mock-oauth.mjs LOG
//
// Prints its port on stdout. Each refresh returns access-<n>/refresh-<n>, valid for an hour,
// and appends "<grant_type> <refresh_token>" to LOG. A refresh token named "rt-revoked" gets a
// 400 invalid_grant.
import { appendFileSync } from "node:fs";
import http from "node:http";

const log = process.argv[2];
let issued = 0;
const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (chunk) => (body += chunk));
  req.on("end", () => {
    const form = new URLSearchParams(body);
    appendFileSync(log, `${form.get("grant_type")} ${form.get("refresh_token")}\n`);
    res.setHeader("content-type", "application/json");
    if (req.method !== "POST" || req.url !== "/v1/oauth/token") {
      res.statusCode = 404;
      res.end("{}");
    } else if (form.get("refresh_token") === "rt-revoked") {
      res.statusCode = 400;
      res.end(JSON.stringify({ error: "invalid_grant", error_description: "the refresh token was revoked" }));
    } else {
      issued += 1;
      res.end(
        JSON.stringify({
          access_token: `access-${issued}`,
          refresh_token: `refresh-${issued}`,
          expires_in: 3600,
        }),
      );
    }
  });
});
server.listen(0, "127.0.0.1", () => console.log(server.address().port));
