import { expect, test } from "@playwright/test"

test("login is Google-only and public account creation is unavailable", async ({ page, request }) => {
  await page.goto("/login")
  await expect(page.getByRole("button", { name: "Continue with Google" })).toBeVisible()
  await expect(page.getByRole("link", { name: /register|sign up|forgot password/i })).toHaveCount(0)

  for (const route of ["register", "login", "forgot-password", "reset-password", "resend-verify"]) {
    const response = await request.post(`/api/v1/auth/${route}`)
    expect(response.status(), route).toBe(404)
  }
  const dns = await request.get("/api/v1/devices/dns-check?name=probe.vpn.local")
  expect(dns.status()).toBe(404)
})

test("an invalid invitation does not grant a session", async ({ page, request }) => {
  const response = await request.post("/api/v1/auth/invitations/verify", {
    data: { token: "invalid-invalid-invalid-invalid" },
  })
  expect(response.ok()).toBe(false)
  await page.goto("/app")
  await expect(page).toHaveURL(/\/login/)
})
