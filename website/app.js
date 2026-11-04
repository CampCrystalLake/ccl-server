document.getElementById("year").textContent = new Date().getFullYear();

document.getElementById("epic-login").addEventListener("click", () => {
  window.alert("تسجيل الدخول عبر Epic لم يُفعّل بعد. سيتم تفعيله بعد إعداد OAuth وربط Callback آمن بخادم KLAY.");
});
