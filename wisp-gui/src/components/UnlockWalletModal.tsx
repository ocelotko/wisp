import { useState } from "react";
import Button from "./Button";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  walletName: string;
  onClose: () => void;
  onSuccess: () => void;
}

export const UnlockWalletModal = ({
  walletName,
  onClose,
  onSuccess,
}: Props) => {
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const handleUnlock = async () => {
    setLoading(true);
    setError("");
    try {
      await invoke("unlock_wallet", { name: walletName, password });
      onSuccess();
    } catch (err) {
      setError("Incorrect password. Please try again.");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-110 flex items-center justify-center bg-black/60 backdrop-blur-md p-4">
      <div className="w-full max-w-md bg-dark-surfaceContainerHigh rounded-4xl border border-dark-outlineVariant p-8 shadow-2xl animate-in zoom-in duration-200">
        <div className="size-12 bg-dark-primary/10 text-dark-primary rounded-2xl flex items-center justify-center mb-6">
          <svg
            xmlns="http://www.w3.org/2000/svg"
            fill="none"
            viewBox="0 0 24 24"
            strokeWidth={1.5}
            stroke="currentColor"
            className="size-6"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              d="M16.5 10.5V6.75a4.5 4.5 0 1 0-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H6.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25Z"
            />
          </svg>
        </div>

        <h3 className="text-2xl font-bold mb-2">Unlock {walletName}</h3>
        <p className="text-dark-onSurfaceVariant mb-8">
          Enter your password to access your Wisp keys.
        </p>

        <input
          autoFocus
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleUnlock()}
          className="w-full bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 mb-2 text-dark-onSurface focus:outline-none focus:border-dark-primary"
          placeholder="Password"
        />

        {error && (
          <p className="text-red-400 text-xs font-medium mt-2">{error}</p>
        )}

        <div className="flex justify-end gap-3 mt-10">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={handleUnlock}
            disabled={loading || !password}
          >
            {loading ? "Unlocking..." : "Unlock Wallet"}
          </Button>
        </div>
      </div>
    </div>
  );
};
