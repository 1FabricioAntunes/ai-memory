import { request } from "./api.js";
import { Field, primary, card } from "./ui.jsx";
import { MIN_PASSWORD_CHARACTERS } from "./policy.js";

export function Login({ run, refresh, busy }) {
  return (
    <section className={`${card} login-panel mx-auto max-w-md`}>
      <h1 className="section-heading mb-2 text-2xl font-semibold">Sign in</h1>
      <p className="mb-6 text-sm text-muted">
        Use your ai-memory username and password.
      </p>
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          const form = e.currentTarget;
          const data = new FormData(form);
          run(async () => {
            await request("/auth/login", {
              method: "POST",
              body: {
                username: data.get("username"),
                password: data.get("password"),
              },
            });
            form.reset();
            await refresh();
          });
        }}
      >
        <Field
          label="Username"
          name="username"
          autoComplete="username"
          required
        />
        <Field
          label="Password"
          name="password"
          type="password"
          autoComplete="current-password"
          required
        />
        <button className={primary} disabled={busy}>
          Sign in
        </button>
      </form>
    </section>
  );
}
function validatePasswordInput(event) {
  const input = event.currentTarget;
  input.setCustomValidity(
    Array.from(input.value).length < MIN_PASSWORD_CHARACTERS
      ? `Use at least ${MIN_PASSWORD_CHARACTERS} characters.`
      : "",
  );
}

export function Password({ run, refresh, busy, required }) {
  return (
    <section className={`${card} max-w-lg`}>
      <h1 className="section-heading mb-2 text-2xl font-semibold">
        Change password
      </h1>
      <p className="mb-6 text-sm text-muted">
        {required
          ? "Choose a new password before using the console."
          : "Changing your password signs out your other browser sessions."}
      </p>
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          const form = e.currentTarget;
          const data = new FormData(form);
          run(async () => {
            await request("/auth/password", {
              method: "POST",
              body: {
                current_password: data.get("current"),
                new_password: data.get("new"),
                new_password_confirmation: data.get("confirmation"),
              },
            });
            form.reset();
            await refresh();
          });
        }}
      >
        <Field
          label="Current password"
          name="current"
          type="password"
          autoComplete="current-password"
          required
        />
        <Field
          label={`New password (at least ${MIN_PASSWORD_CHARACTERS} characters)`}
          name="new"
          minLength={MIN_PASSWORD_CHARACTERS}
          onInput={validatePasswordInput}
          type="password"
          autoComplete="new-password"
          required
        />
        <Field
          label="Confirm new password"
          name="confirmation"
          minLength={MIN_PASSWORD_CHARACTERS}
          onInput={validatePasswordInput}
          type="password"
          autoComplete="new-password"
          required
        />
        <button className={primary} disabled={busy}>
          Change password
        </button>
      </form>
    </section>
  );
}
