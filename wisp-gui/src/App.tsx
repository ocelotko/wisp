import { useEffect, useState } from "react";
import ActivityItem from "./components/ActivityItem";
import Button from "./components/Button";
import IconButton from "./components/IconButton";
import { invoke } from "@tauri-apps/api/core";
import { CreateWalletModal } from "./components/CreateWalletModal";
import { RecoverWalletModal } from "./components/RecoverWalletModal";
import { UnlockWalletModal } from "./components/UnlockWalletModal";

type View = "home" | "settings";

const App = () => {
  const [currentView, setCurrentView] = useState<View>("home");
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [showAddMenu, setShowAddMenu] = useState(false);
  const [activeModal, setActiveModal] = useState<"create" | "recover" | null>(
    null,
  );
  const [wallets, setWallets] = useState<string[]>([]);
  const [activeWallet, setActiveWallet] = useState<string | null>(null);
  const [pendingWallet, setPendingWallet] = useState<string | null>(null);
  const [unlockedWallet, setUnlockedWallet] = useState<string | null>(null);
  const [balance, setBalance] = useState<number>(0);

  const loadWallets = async () => {
    try {
      const list = await invoke<string[]>("list_wallets");
      setWallets(list);
      if (!activeWallet && list.length > 0) setActiveWallet(list[0]);
    } catch (err) {
      console.error("Failed to load wallets:", err);
    }
  };

  const handleRefresh = async () => {
    setIsRefreshing(true);
    try {
      await invoke("refresh_wallet");
      console.log("Wallet state updated!");
    } catch (err) {
      console.error("Failed to sync:", err);
    } finally {
      setTimeout(() => setIsRefreshing(false), 600);
    }
  };

  useEffect(() => {
    loadWallets();

    if (activeWallet) {
      const fetchBalance = async () => {
        try {
          const b = await invoke<number>("get_wallet_balance", {
            name: activeWallet,
          });
          setBalance(b);
        } catch (e) {
          console.error("Balance fetch failed", e);
        }
      };
      fetchBalance();
    }
  }, [activeWallet]);

  const SendIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
      className="size-5"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="m4.5 19.5 15-15m0 0H8.25m11.25 0v11.25"
      />
    </svg>
  );

  const ReceiveIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
      className="size-5"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="m19.5 4.5-15 15m0 0h11.25m-11.25 0V8.25"
      />
    </svg>
  );

  const RefreshIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"
      />
    </svg>
  );

  const SettingsIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 0 1 1.37.49l1.296 2.247a1.125 1.125 0 0 1-.26 1.431l-1.003.827c-.293.241-.438.613-.43.992a7.723 7.723 0 0 1 0 .255c-.008.378.137.75.43.991l1.004.827c.424.35.534.955.26 1.43l-1.298 2.247a1.125 1.125 0 0 1-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.47 6.47 0 0 1-.22.128c-.331.183-.581.495-.644.869l-.213 1.281c-.09.543-.56.94-1.11.94h-2.594c-.55 0-1.019-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 0 1-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 0 1-1.369-.49l-1.297-2.247a1.125 1.125 0 0 1 .26-1.431l1.004-.827c.292-.24.437-.613.43-.991a6.932 6.932 0 0 1 0-.255c.007-.38-.138-.751-.43-.992l-1.004-.827a1.125 1.125 0 0 1-.26-1.43l1.297-2.247a1.125 1.125 0 0 1 1.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.086.22-.128.332-.183.582-.495.644-.869l.214-1.281Z"
      />
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z"
      />
    </svg>
  );

  const PlusIcon = (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      fill="none"
      viewBox="0 0 24 24"
      strokeWidth={1.5}
      stroke="currentColor"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M12 4.5v15m7.5-7.5h-15"
      />
    </svg>
  );

  return (
    <>
      {showAddMenu && (
        <div
          className="fixed inset-0 z-40 bg-transparent"
          onClick={() => setShowAddMenu(false)}
        />
      )}

      <div className="flex w-full h-screen overflow-hidden bg-dark-background font-dmsans text-dark-onSurface relative">
        <div
          data-tauri-drag-region
          className="absolute top-0 left-0 w-full h-8 z-50 cursor-default select-none"
        />

        <aside className="w-70 bg-dark-surfaceContainerLow flex flex-col p-6 border-r border-dark-outlineVariant pt-10">
          <header className="text-dark-primary text-2xl font-black uppercase mb-8 tracking-tighter">
            Wisp wallet
          </header>

          <nav className="flex-1">
            <ul className="space-y-2">
              <li
                onClick={() => setCurrentView("home")}
                className={`px-4 py-2 rounded-full cursor-pointer transition-colors font-medium ${
                  currentView === "home"
                    ? "bg-dark-secondaryContainer text-dark-onSecondaryContainer"
                    : "hover:bg-dark-surfaceVariant text-dark-onSurfaceVariant"
                }`}
              >
                Home
              </li>
              <li
                onClick={() => setCurrentView("settings")}
                className={`px-4 py-2 rounded-full cursor-pointer transition-colors font-medium ${
                  currentView === "settings"
                    ? "bg-dark-secondaryContainer text-dark-onSecondaryContainer"
                    : "hover:bg-dark-surfaceVariant text-dark-onSurfaceVariant"
                }`}
              >
                Settings
              </li>
            </ul>
            <section className="mt-10 relative">
              <div className="flex items-center justify-between px-4 mb-4">
                <h4 className="text-[10px] uppercase tracking-widest text-dark-outline font-bold">
                  Accounts
                </h4>
                <IconButton
                  icon={PlusIcon}
                  onClick={() => setShowAddMenu(!showAddMenu)}
                  className={`size-8 ${showAddMenu ? "bg-dark-primary/20 text-dark-primary" : ""}`}
                />
              </div>

              {showAddMenu && (
                <div className="absolute right-0 top-10 w-64 bg-dark-surfaceContainerHigh border border-dark-outlineVariant rounded-2xl shadow-2xl z-50 p-2 animate-in fade-in zoom-in duration-200">
                  <button
                    onClick={() => {
                      setActiveModal("create");
                      setShowAddMenu(false); // Close menu when modal opens
                    }}
                    className="w-full text-left px-4 py-3 rounded-xl hover:bg-dark-surfaceVariant flex flex-col transition-colors"
                  >
                    <span className="text-sm font-bold text-dark-onSurface">
                      Create a new wallet
                    </span>
                    <span className="text-[10px] text-dark-outline">
                      Generates a new 24-word seed
                    </span>
                  </button>

                  <button
                    onClick={() => {
                      setActiveModal("recover");
                      setShowAddMenu(false); // Close menu when modal opens
                    }}
                    className="w-full text-left px-4 py-3 rounded-xl hover:bg-dark-surfaceVariant flex flex-col mt-1 transition-colors"
                  >
                    <span className="text-sm font-bold text-dark-onSurface">
                      Recover wallet
                    </span>
                    <span className="text-[10px] text-dark-outline">
                      Use a seed phrase or private key
                    </span>
                  </button>
                </div>
              )}

              <div className="space-y-1">
                {wallets.map((walletName) => (
                  <button
                    key={walletName}
                    onClick={() => {
                      if (unlockedWallet === walletName) {
                        setActiveWallet(walletName);
                      } else {
                        setPendingWallet(walletName);
                      }
                    }}
                    className={`...`}
                  >
                    {walletName}
                  </button>
                ))}

                {wallets.length === 0 && (
                  <p className="px-4 text-xs text-dark-outline italic">
                    No wallets found
                  </p>
                )}
              </div>
            </section>
          </nav>

          <footer className="px-4 text-xs text-dark-outline opacity-50">
            balls
          </footer>
        </aside>

        <main className="flex-1 p-12 overflow-y-auto pt-14">
          {currentView === "home" ? (
            <>
              <header className="flex justify-between items-start mb-12">
                <div>
                  <h2 className="text-4xl font-medium mb-1">Wallet 1</h2>
                  <p className="text-dark-onSurfaceVariant">
                    Legacy address format
                  </p>
                </div>

                <div className="flex items-center gap-2">
                  {" "}
                  {/* Container for all actions */}
                  <div className="flex gap-3 mr-4">
                    {" "}
                    <Button variant="primary" icon={SendIcon}>
                      Send
                    </Button>
                    <Button variant="ghost" icon={ReceiveIcon}>
                      Receive
                    </Button>
                  </div>
                  <div className="flex gap-1">
                    <IconButton
                      icon={RefreshIcon}
                      onClick={handleRefresh}
                      className={
                        isRefreshing ? "animate-spin text-dark-primary" : ""
                      }
                    />
                    <IconButton
                      icon={SettingsIcon}
                      onClick={() => console.log("Settings")}
                    />
                  </div>
                </div>
              </header>

              <section className="mb-12">
                <p className="text-sm text-dark-onSurfaceVariant mb-2 font-medium">
                  Current balance:
                </p>
                <div className="text-6xl font-light tracking-tight">
                  0.00{" "}
                  <span className="text-dark-outline text-3xl ml-1">WISP</span>
                </div>
                <div className="mt-10 h-72 w-full bg-dark-surfaceContainer rounded-3xl border border-dark-outlineVariant flex items-center justify-center relative overflow-hidden">
                  <div className="absolute inset-0 bg-linear-to-b from-dark-primary/5 to-transparent" />
                  <span className="text-dark-outline font-medium">
                    Chart visualization area
                  </span>
                </div>
              </section>

              <section>
                <h3 className="text-xl font-medium mb-6">Recent Activity</h3>
                <div className="flex flex-col">
                  <p className="text-xs font-bold text-dark-outline uppercase tracking-widest mb-4">
                    August 05, 2020
                  </p>
                  <ActivityItem
                    type="Sent"
                    time="10:54 PM"
                    address="Kokot"
                    amount="0.001"
                  />
                  <ActivityItem
                    type="Received"
                    time="09:12 AM"
                    address="Pica"
                    amount="0.052"
                  />
                </div>
              </section>
            </>
          ) : (
            <div className="animate-in fade-in slide-in-from-bottom-4 duration-500">
              <header className="mb-12">
                <h2 className="text-4xl font-medium mb-1">Settings</h2>
                <p className="text-dark-onSurfaceVariant">
                  Configure your Wisp node and security
                </p>
              </header>

              <section className="space-y-6 max-w-2xl">
                <div className="p-6 bg-dark-surfaceContainer rounded-3xl border border-dark-outlineVariant">
                  <h4 className="text-lg font-bold mb-4">Node Connection</h4>
                  <div className="space-y-4">
                    <div>
                      <label className="text-xs font-bold text-dark-outline uppercase ml-1">
                        Default Node Address
                      </label>
                      <input
                        className="w-full mt-1 bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors"
                        placeholder="0.0.0.0:9000"
                      />
                    </div>
                  </div>
                </div>

                <div className="p-6 bg-dark-surfaceContainer rounded-3xl border border-dark-outlineVariant">
                  <h4 className="text-lg font-bold mb-4 text-red-400">
                    Danger Zone
                  </h4>
                  <Button
                    variant="ghost"
                    className="text-red-400 hover:bg-red-400/10"
                  >
                    Delete All Wallets
                  </Button>
                </div>
              </section>
            </div>
          )}
        </main>
      </div>

      {activeModal === "create" && (
        <CreateWalletModal
          onClose={() => setActiveModal(null)}
          onSuccess={() => {
            loadWallets();
          }}
        />
      )}

      {activeModal === "recover" && (
        <RecoverWalletModal
          onClose={() => setActiveModal(null)}
          onSuccess={() => {
            loadWallets();
          }}
        />
      )}

      {pendingWallet && (
        <UnlockWalletModal
          walletName={pendingWallet}
          onClose={() => setPendingWallet(null)}
          onSuccess={() => {
            setUnlockedWallet(pendingWallet);
            setActiveWallet(pendingWallet);
            setPendingWallet(null);
            handleRefresh();
          }}
        />
      )}
    </>
  );
};

export default App;
