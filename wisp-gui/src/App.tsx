import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import Button from "./components/Button.tsx";
import "./App.css";

// --- Helper Components ---

const Input = ({ value, onChange, placeholder, type = "text", label, className = "" }: any) => (
  <div className={`flex flex-col gap-2 ${className}`}>
    {label && <label className="text-xs font-bold uppercase tracking-wider text-dark-onSurfaceVariant ml-1">{label}</label>}
    <input
      type={type}
      value={value}
      onChange={onChange}
      placeholder={placeholder}
      className="w-full bg-dark-surfaceContainerHigh text-dark-onSurface px-4 py-4 rounded-2xl border-2 border-transparent focus:border-dark-primary focus:outline-none transition-colors placeholder:text-dark-onSurfaceVariant/50 font-medium"
    />
  </div>
);

const Modal = ({ title, onClose, children }: any) => (
  <div className="fixed inset-0 z-50 flex items-end sm:items-center justify-center p-4 sm:p-6">
    <div className="absolute inset-0 bg-dark-scrim/80 backdrop-blur-sm transition-opacity" onClick={onClose} />
    <div className="relative w-full max-w-md bg-dark-surfaceContainer rounded-3xl p-6 shadow-2xl animate-in slide-in-from-bottom-10 fade-in duration-200">
      <div className="flex justify-between items-center mb-6">
        <h2 className="text-xl font-bold text-dark-onSurface">{title}</h2>
        <button onClick={onClose} className="p-2 rounded-full hover:bg-dark-surfaceContainerHighest text-dark-onSurfaceVariant hover:text-dark-onSurface transition-colors">
          <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-6 h-6"><path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
        </button>
      </div>
      {children}
    </div>
  </div>
);

interface Transaction {
  id: string;
  kind: "Sent" | "Received" | "Coinbase";
  amount: string;
  timestamp: string;
  status: "Pending" | "Confirmed";
}

// --- Main App ---

function App() {
  const [view, setView] = useState<"loading" | "auth" | "dashboard">("loading");
  const [wallets, setWallets] = useState<string[]>([]);
  const [walletName, setWalletName] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [isCreating, setIsCreating] = useState(false);

  // Dashboard State
  const [balance, setBalance] = useState("0.00");
  const [transactions, setTransactions] = useState<Transaction[]>([]);
  const [address, setAddress] = useState("");
  const [showSend, setShowSend] = useState(false);
  const [showReceive, setShowReceive] = useState(false);

  // Send Form State
  const [recipient, setRecipient] = useState("");
  const [amount, setAmount] = useState("");
  const [isSendMax, setIsSendMax] = useState(false);
  const [fee, setFee] = useState("100"); // Default fixed fee
  const [sendPassword, setSendPassword] = useState("");

  useEffect(() => {
    checkWallets();
  }, []);

  async function checkWallets() {
    try {
      const list: string[] = await invoke("list_wallets");
      setWallets(list);
      if (list.length > 0) {
        setWalletName(list[0]);
        setIsCreating(false);
      } else {
        setIsCreating(true);
      }
      setView("auth");
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleAuth() {
    setError("");
    try {
      if (isCreating) {
        await invoke("create_wallet", { name: walletName, password });
      } else {
        await invoke("load_wallet", { name: walletName, password });
      }
      await refreshWalletData();
      setView("dashboard");
      setPassword(""); // Clear auth password
    } catch (e) {
      setError(String(e));
    }
  }

  async function refreshWalletData() {
    try {
      const bal: string = await invoke("get_balance");
      const addr: string = await invoke("get_address");
      const txs: Transaction[] = await invoke("get_transactions");
      setBalance(bal);
      setTransactions(txs);
      setAddress(addr);
    } catch (e) {
      console.error("Failed to fetch wallet data", e);
    }
  }

  async function handleSend() {
    setError("");
    try {
      await invoke("send_funds", {
        recipient,
        amountWisp: amount,
        feeFixed: parseInt(fee),
        password: sendPassword,
        isSendMax
      });
      setShowSend(false);
      setRecipient("");
      setAmount("");
      setSendPassword("");
      // Refresh balance after a short delay to allow propagation
      setTimeout(refreshWalletData, 1000);
    } catch (e) {
      setError(String(e));
    }
  }

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text);
    // Optional: Add toast here
  };

  if (view === "loading") {
    return <div className="min-h-screen flex items-center justify-center text-dark-primary animate-pulse">Loading Wisp...</div>;
  }

  if (view === "auth") {
    return (
      <div className="min-h-screen flex flex-col items-center justify-center p-6 max-w-md mx-auto w-full">
        <div className="mb-12 text-center">
          <h1 className="text-4xl font-bold text-dark-onBackground mb-2">Wisp</h1>
          <p className="text-dark-onSurfaceVariant">Minimalist Privacy Wallet</p>
        </div>

        <div className="w-full space-y-4">
          {isCreating ? (
            <Input 
              label="Wallet Name" 
              value={walletName} 
              onChange={(e: any) => setWalletName(e.target.value)} 
              placeholder="e.g. Main Wallet" 
            />
          ) : (
            <div className="flex flex-col gap-2">
              <label className="text-xs font-bold uppercase tracking-wider text-dark-onSurfaceVariant ml-1">Select Wallet</label>
              <select 
                value={walletName} 
                onChange={(e) => setWalletName(e.target.value)}
                className="w-full bg-dark-surfaceContainerHigh text-dark-onSurface px-4 py-4 rounded-2xl border-2 border-transparent focus:border-dark-primary focus:outline-none appearance-none font-medium"
              >
                {wallets.map(w => <option key={w} value={w}>{w}</option>)}
              </select>
            </div>
          )}

          <Input 
            label="Password" 
            type="password" 
            value={password} 
            onChange={(e: any) => setPassword(e.target.value)} 
            placeholder="Enter your password" 
          />

          {error && <div className="p-4 rounded-xl bg-dark-errorContainer text-dark-onError text-sm font-medium">{error}</div>}

          <Button onClick={handleAuth} className="w-full mt-4">
            {isCreating ? "Create Wallet" : "Unlock Wallet"}
          </Button>

          <button 
            onClick={() => { setIsCreating(!isCreating); setError(""); }}
            className="w-full text-center text-sm text-dark-primary font-bold py-2 hover:underline"
          >
            {isCreating ? "I already have a wallet" : "Create a new wallet"}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-md mx-auto w-full relative">
      {/* Header / Status */}
      <div className="flex justify-between items-center py-4">
        <div className="flex items-center gap-2 px-3 py-1.5 bg-dark-surfaceContainer rounded-full">
          <div className="w-2 h-2 rounded-full bg-emerald-400 animate-pulse shadow-[0_0_8px_rgba(52,211,153,0.6)]"></div>
          <span className="text-xs font-bold text-dark-onSurfaceVariant uppercase tracking-wide">Synced</span>
        </div>
        <button onClick={() => setView("auth")} className="text-dark-onSurfaceVariant hover:text-dark-onSurface">
          <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-6 h-6"><path strokeLinecap="round" strokeLinejoin="round" d="M15.75 9V5.25A2.25 2.25 0 0013.5 3h-6a2.25 2.25 0 00-2.25 2.25v13.5A2.25 2.25 0 007.5 21h6a2.25 2.25 0 002.25-2.25V15M12 9l-3 3m0 0l3 3m-3-3h12.75" /></svg>
        </button>
      </div>

      {/* Balance Display */}
      <div className="flex-1 flex flex-col items-center justify-center min-h-[40vh]">
        <div className="text-dark-onSurfaceVariant font-medium mb-2">Total Balance</div>
        <div className="text-6xl font-bold text-dark-onBackground tracking-tight mb-1">
          {balance}
        </div>
        <div className="text-xl text-dark-primary font-medium">WISP</div>
      </div>

      {/* Action Buttons */}
      <div className="grid grid-cols-2 gap-4 mb-8">
        <Button variant="secondary" onClick={() => setShowSend(true)} className="flex-col py-6 gap-3" icon={<svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-8 h-8"><path strokeLinecap="round" strokeLinejoin="round" d="M4.5 19.5l15-15m0 0H8.25m11.25 0v11.25" /></svg>}>
          Send
        </Button>
        <Button variant="primary" onClick={() => setShowReceive(true)} className="flex-col py-6 gap-3" icon={<svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-8 h-8"><path strokeLinecap="round" strokeLinejoin="round" d="M19.5 19.5l-15-15m0 0v11.25m0-11.25h11.25" /></svg>}>
          Receive
        </Button>
      </div>

      {/* Recent Activity Placeholder */}
      <div className="flex-1">
        <h3 className="text-sm font-bold text-dark-onSurfaceVariant uppercase tracking-wider mb-4">Recent Activity</h3>
        
        {transactions.length === 0 ? (
          <div className="p-8 text-center border-2 border-dashed border-dark-outlineVariant rounded-3xl text-dark-onSurfaceVariant">
            No recent transactions
          </div>
        ) : (
          <div className="space-y-3 pb-4">
            {transactions.map((tx) => (
              <div key={tx.id} className="flex justify-between items-center p-4 bg-dark-surfaceContainerHigh/50 rounded-2xl border border-transparent hover:border-dark-outlineVariant transition-colors">
                <div className="flex items-center gap-4">
                  <div className={`w-10 h-10 rounded-full flex items-center justify-center ${tx.kind === 'Received' || tx.kind === 'Coinbase' ? 'bg-emerald-500/20 text-emerald-400' : 'bg-dark-surfaceContainerHighest text-dark-onSurface'}`}>
                    {tx.kind === 'Received' || tx.kind === 'Coinbase' ? (
                      <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-5 h-5"><path strokeLinecap="round" strokeLinejoin="round" d="M19.5 13.5L12 21m0 0l-7.5-7.5M12 21V3" /></svg>
                    ) : (
                      <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-5 h-5"><path strokeLinecap="round" strokeLinejoin="round" d="M4.5 10.5L12 3m0 0l7.5 7.5M12 3v18" /></svg>
                    )}
                  </div>
                  <div>
                    <p className="font-bold text-dark-onSurface">{tx.kind}</p>
                    <p className="text-xs text-dark-onSurfaceVariant">{new Date(tx.timestamp).toLocaleDateString()} • {new Date(tx.timestamp).toLocaleTimeString([], {hour: '2-digit', minute:'2-digit'})}</p>
                  </div>
                </div>
                <div className="text-right">
                  <p className={`font-bold ${tx.kind === 'Received' || tx.kind === 'Coinbase' ? 'text-emerald-400' : 'text-dark-onSurface'}`}>{tx.amount} WISP</p>
                  <p className={`text-xs font-medium ${tx.status === 'Pending' ? 'text-amber-400' : 'text-dark-onSurfaceVariant'}`}>{tx.status}</p>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Send Modal */}
      {showSend && (
        <Modal title="Send Wisp" onClose={() => setShowSend(false)}>
          <div className="space-y-4">
            <Input 
              label="Recipient Address" 
              placeholder="Public Key (Hex)" 
              value={recipient} 
              onChange={(e: any) => setRecipient(e.target.value)} 
            />
            <div className="flex gap-4 items-end">
              <div className="flex-1 relative">
                <Input 
                  label="Amount" 
                  placeholder="0.00" 
                  value={amount} 
                  onChange={(e: any) => { setAmount(e.target.value); setIsSendMax(false); }} 
                  className="w-full"
                />
                <button 
                  onClick={() => { setIsSendMax(true); setAmount("MAX"); }}
                  className="absolute right-3 top-[38px] text-xs font-bold text-dark-primary hover:text-dark-onPrimary bg-dark-surfaceContainer px-2 py-1 rounded-md transition-colors"
                >
                  MAX
                </button>
              </div>
              <div className="w-1/3">
                 <label className="text-xs font-bold uppercase tracking-wider text-dark-onSurfaceVariant ml-1">Asset</label>
                 <div className="w-full bg-dark-surfaceContainerHigh text-dark-onSurface px-4 py-4 rounded-2xl border-2 border-transparent font-bold text-center mt-2">WISP</div>
              </div>
            </div>
            <Input 
              label="Wallet Password" 
              type="password" 
              placeholder="Required to sign" 
              value={sendPassword} 
              onChange={(e: any) => setSendPassword(e.target.value)} 
            />
            
            {error && <div className="p-3 rounded-xl bg-dark-errorContainer text-dark-onError text-sm">{error}</div>}
            
            <Button onClick={handleSend} className="w-full mt-4">Confirm Send</Button>
          </div>
        </Modal>
      )}

      {/* Receive Modal */}
      {showReceive && (
        <Modal title="Receive Wisp" onClose={() => setShowReceive(false)}>
          <div className="flex flex-col items-center gap-6 py-4">
            <div className="w-48 h-48 bg-white rounded-2xl flex items-center justify-center">
              {/* Placeholder for QR Code */}
              <div className="text-black font-bold opacity-20">QR CODE</div>
            </div>
            <div className="w-full">
              <label className="text-xs font-bold uppercase tracking-wider text-dark-onSurfaceVariant ml-1 mb-2 block">Your Address</label>
              <div 
                onClick={() => copyToClipboard(address)}
                className="bg-dark-surfaceContainerHigh p-4 rounded-2xl break-all font-mono text-sm text-dark-onSurface border-2 border-transparent hover:border-dark-primary cursor-pointer transition-colors flex gap-2"
              >
                {address}
                <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="w-5 h-5 shrink-0"><path strokeLinecap="round" strokeLinejoin="round" d="M15.75 17.25v3.375c0 .621-.504 1.125-1.125 1.125h-9.75a1.125 1.125 0 01-1.125-1.125V7.875c0-.621.504-1.125 1.125-1.125H6.75a9.06 9.06 0 011.5.124m7.5 10.376h3.375c.621 0 1.125-.504 1.125-1.125V11.25c0-4.46-3.243-8.161-7.5-8.876a9.06 9.06 0 00-1.5-.124H9.375c-.621 0-1.125.504-1.125 1.125v3.5m7.5 10.375H9.375a1.125 1.125 0 01-1.125-1.125v-9.25m12 6.625v-1.875a3.375 3.375 0 00-3.375-3.375h-1.5" /></svg>
              </div>
              <p className="text-center text-dark-onSurfaceVariant text-xs mt-2">Tap address to copy</p>
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}

export default App;
