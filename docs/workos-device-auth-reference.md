# WorkOS Device Authorization Flow Reference

For implementing the device auth flow in the install script (pure bash + curl + jq).

## Endpoints

All endpoints use `application/x-www-form-urlencoded` request bodies. No auth headers needed (public client).

### 1. Initiate

```
POST https://api.workos.com/user_management/authorize/device
```

**Parameters:** `client_id` (required)

**Response:**
```json
{
  "device_code": "71azDp28ToiCscGDvLxnXLkuuFRMrnd4V7rdsjIlBPXuy13j8GOzU0aZHb46tsz3",
  "user_code": "RRGQ-BJVS",
  "verification_uri": "https://<authkit_domain>/device",
  "verification_uri_complete": "https://<authkit_domain>/device?user_code=ABCD-EFGH",
  "expires_in": 300,
  "interval": 5
}
```

### 2. Poll for tokens

```
POST https://api.workos.com/user_management/authenticate
```

**Parameters:**
- `grant_type`: `urn:ietf:params:oauth:grant-type:device_code`
- `device_code`: from step 1
- `client_id`: same client ID

**Polling errors** (non-200, check `error` field in JSON body):

| `error` value | Action |
|---|---|
| `authorization_pending` | Wait `interval` seconds, poll again |
| `slow_down` | Increase interval by 1 second |
| `access_denied` | Stop — user denied |
| `expired_token` | Stop — device_code expired (300s) |

**Success response** (HTTP 200):
```json
{
  "user": {
    "id": "user_...",
    "email": "user@example.com",
    "first_name": "Name",
    "last_name": "Last"
  },
  "access_token": "<JWT>",
  "refresh_token": "<opaque string>"
}
```

### 3. Token refresh (for auth status check)

```
POST https://api.workos.com/user_management/authenticate
```

**Parameters:**
- `grant_type`: `refresh_token`
- `refresh_token`: stored refresh token
- `client_id`: same client ID

Returns same format as success response above. Refresh tokens are single-use (rotated on each refresh).

## Client ID

Public value (no secret needed for device flow). Configurable via `ALIGNMENT_HIVE_CLIENT_ID` or `HIVE_MIND_CLIENT_ID` environment variables.

## Bash implementation sketch

```bash
# Initiate
RESPONSE=$(curl -s -X POST https://api.workos.com/user_management/authorize/device \
  -d "client_id=$CLIENT_ID")

DEVICE_CODE=$(echo "$RESPONSE" | jq -r '.device_code')
USER_CODE=$(echo "$RESPONSE" | jq -r '.user_code')
VERIFICATION_URI=$(echo "$RESPONSE" | jq -r '.verification_uri_complete')
INTERVAL=$(echo "$RESPONSE" | jq -r '.interval')
EXPIRES_IN=$(echo "$RESPONSE" | jq -r '.expires_in')

# Open browser
open "$VERIFICATION_URI" 2>/dev/null || xdg-open "$VERIFICATION_URI" 2>/dev/null

# Poll
DEADLINE=$((SECONDS + EXPIRES_IN))
while [ $SECONDS -lt $DEADLINE ]; do
  sleep "$INTERVAL"
  TOKEN_RESPONSE=$(curl -s -X POST https://api.workos.com/user_management/authenticate \
    -d "grant_type=urn:ietf:params:oauth:grant-type:device_code" \
    -d "device_code=$DEVICE_CODE" \
    -d "client_id=$CLIENT_ID")

  ERROR=$(echo "$TOKEN_RESPONSE" | jq -r '.error // empty')
  if [ -z "$ERROR" ]; then
    # Success — save TOKEN_RESPONSE to auth file
    break
  elif [ "$ERROR" = "slow_down" ]; then
    INTERVAL=$((INTERVAL + 1))
  elif [ "$ERROR" != "authorization_pending" ]; then
    # Terminal error
    break
  fi
done
```
