const year = document.getElementById("year");
if (year) year.textContent = String(new Date().getFullYear());

const loginButton = document.getElementById("epic-login");
if (loginButton) {
  loginButton.addEventListener("click", () => {
    loginButton.disabled = true;
    loginButton.textContent = "جارٍ تحويلك إلى Epic...";
    window.location.assign("/auth/epic/login");
  });
}

async function loadEpicAccount() {
  try {
    const response = await fetch("/auth/epic/me", { credentials: "same-origin" });
    if (!response.ok) return;
    const account = await response.json();
    const status = document.getElementById("epic-account");
    if (status && account.success) {
      status.textContent = "مسجّل الدخول: " + account.displayName;
      status.hidden = false;
    }
  } catch (error) {
    console.error("Could not load KLAY account:", error);
  }
}
loadEpicAccount();
