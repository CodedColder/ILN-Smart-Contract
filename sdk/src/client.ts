/**
 * ILNClient — entry point for the Invoice Liquidity Network SDK.
 *
 * Provides factory methods for common environments so integrators can
 * get started with a one-liner:
 *
 * ```ts
 * import { ILNClient } from "@iln/sdk";
 *
 * const client = ILNClient.testnet(signer);
 * const reputation = await client.getReputation("G...");
 * ```
 *
 * ## Architecture
 *
 * `ILNClient` is a thin wrapper around the SDK's free functions. It holds
 * the RPC server, network passphrase, contract address, and signer so
 * every method call uses the same configuration automatically.
 */

import { SorobanRpc } from "@stellar/stellar-sdk";
import type { ISigner } from "./signers/ISigner.js";

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/**
 * Public Soroban RPC endpoint for Stellar Testnet.
 */
export const TESTNET_RPC_URL = "https://soroban-testnet.stellar.org";

/**
 * Public Soroban RPC endpoint for Stellar Mainnet (Pubnet).
 */
export const MAINNET_RPC_URL = "https://soroban.stellar.org";

// ---------------------------------------------------------------------------
// Contract registry
// ---------------------------------------------------------------------------

/** The five deployed ILN contract addresses for a given network. */
export interface ContractAddresses {
  /** Invoice-liquidity contract address. */
  invoiceLiquidity: string;
  /** Insurance pool contract address. */
  insurancePool: string;
  /** Distribution contract address. */
  distribution: string;
  /** Fund queue contract address. */
  fundQueue: string;
  /** NFT / reputation token contract address. */
  nft: string;
}

/** Known network names. */
export type NetworkName = "testnet" | "mainnet";

/**
 * Per-network contract registry.
 *
 * Testnet addresses are populated from the latest CI/CD deployment.
 * Mainnet addresses are left empty until mainnet deployment; calling
 * `ILNClient.mainnet()` without explicit overrides throws rather than
 * silently using a bogus address (#876).
 */
export const CONTRACT_REGISTRY: Record<NetworkName, ContractAddresses> = {
  testnet: {
    invoiceLiquidity:
      "CCVXGPKFAN374T62PLZAHWIS4UKUVTOYRD72HT36SGWWX7LRD5VFUUJD",
    insurancePool: "",
    distribution: "",
    fundQueue: "",
    nft: "",
  },
  mainnet: {
    // Intentionally left empty — mainnet deployment not yet completed.
    // ILNClient.mainnet() will throw if these are not populated.
    invoiceLiquidity: "",
    insurancePool: "",
    distribution: "",
    fundQueue: "",
    nft: "",
  },
};

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/** Full configuration for a custom ILNClient. */
export interface ILNClientConfig {
  /** Soroban RPC endpoint URL. */
  rpcUrl: string;
  /** Stellar network passphrase (e.g. `Networks.TESTNET`). */
  networkPassphrase: string;
  /** Deployed invoice-liquidity contract address. */
  contractId: string;
  /**
   * Optional signer for methods that require authentication (e.g. fundInvoice).
   * Read-only methods like getReputation work without a signer.
   */
  signer?: ISigner;
  /**
   * Optional pre-populated contract addresses for all five ILN contracts.
   * When provided, overrides the registry lookup for the bound network.
   */
  contracts?: Partial<ContractAddresses>;
}

// ---------------------------------------------------------------------------
// ILNClient
// ---------------------------------------------------------------------------

/**
 * Configured SDK client bound to a specific network and contract.
 *
 * @example
 * ```ts
 * // Testnet
 * const client = ILNClient.testnet(mySigner);
 *
 * // Custom RPC (e.g. local validator node)
 * const client = ILNClient.custom({
 *   rpcUrl: "http://localhost:8000/soroban/rpc",
 *   networkPassphrase: Networks.STANDALONE,
 *   contractId: "CDEPLOYED...",
 *   signer: mySigner,
 * });
 * ```
 */
export class ILNClient {
  /** Soroban RPC server instance. */
  readonly rpc: SorobanRpc.Server;
  /** Stellar network passphrase. */
  readonly networkPassphrase: string;
  /** Deployed invoice-liquidity contract address. */
  readonly contractId: string;
  /** Optional signer for authenticated methods. */
  readonly signer?: ISigner | undefined;
  /** Pre-populated contract addresses for all five ILN contracts. */
  readonly contracts: ContractAddresses;

  // Cached imports (lazy-loaded for tree-shaking)
  private _getReputation?: typeof import("./methods/reputation.js").getReputation;
  private _getContractStats?: typeof import("./methods/stats.js").getContractStats;
  private _getTopPayers?: typeof import("./methods/topPayers.js").getTopPayers;
  private _getLpInvoices?: typeof import("./methods/lpInvoices.js").getLpInvoices;
  private _getPoolBalance?: typeof import("./methods/insurance.js").getPoolBalance;
  private _getCoverage?: typeof import("./methods/insurance.js").getCoverage;
  private _isEnrolled?: typeof import("./methods/insurance.js").isEnrolled;
  private _getPremiumsPaid?: typeof import("./methods/insurance.js").getPremiumsPaid;
  private _getInsurancePoolInfo?: typeof import("./methods/insurance.js").getInsurancePoolInfo;
  private _getPoolHealth?: typeof import("./methods/insurance.js").getPoolHealth;
  private _getDistributionAccrual?: typeof import("./methods/distribution.js").getDistributionAccrual;
  private _submitBatchTransaction?: typeof import("./methods/batch.js").submitBatchTransaction;
  private _networkVerified = false;

  constructor(config: ILNClientConfig) {
    this.rpc = new SorobanRpc.Server(config.rpcUrl);
    this.networkPassphrase = config.networkPassphrase;
    this.contractId = config.contractId;
    this.signer = config.signer;
    this.contracts = {
      invoiceLiquidity: config.contractId,
      insurancePool: config.contracts?.insurancePool ?? "",
      distribution: config.contracts?.distribution ?? "",
      fundQueue: config.contracts?.fundQueue ?? "",
      nft: config.contracts?.nft ?? "",
    };
  }

  // --------------------------------------------------------------------------
  // Network verification
  // --------------------------------------------------------------------------

  /**
   * Verify that the configured network passphrase matches the RPC endpoint.
   *
   * Queries `getNetwork()` on the Soroban RPC to obtain the server's actual
   * network passphrase and compares it against the passphrase configured in
   * this client. On mismatch, throws with a clear diagnostic message so the
   * operator can fix their configuration before submitting any transactions.
   *
   * Call this after construction (or use the `testnet()` / `mainnet()` factory
   * methods which call it automatically) to guard against costly
   * mainnet-contract-against-testnet-RPC mistakes.
   *
   * @throws {Error} if the RPC passphrase does not match the configured passphrase
   *
   * @example
   * ```ts
   * const client = ILNClient.custom({ rpcUrl: "...", networkPassphrase: Networks.TESTNET, contractId: "..." });
   * await client.verifyNetwork(); // throws if RPC is actually mainnet
   * ```
   */
  async verifyNetwork(): Promise<void> {
    try {
      const network = await this.rpc.getNetwork();
      if (network.passphrase !== this.networkPassphrase) {
        throw new Error(
          `Network mismatch: configured passphrase "${this.networkPassphrase}" ` +
          `does not match the RPC endpoint's passphrase "${network.passphrase}". ` +
          `You may be pointing a mainnet contract at a testnet RPC (or vice versa).`
        );
      }
    } catch (err) {
      if (err instanceof Error && err.message.includes("Network mismatch")) {
        throw err;
      }
      throw new Error(
        `Failed to verify network passphrase: ${err instanceof Error ? err.message : String(err)}`
      );
    }
  }

  // --------------------------------------------------------------------------
  // Factory methods
  // --------------------------------------------------------------------------

  /**
   * Create a client pre-configured for Stellar Testnet.
   *
   * @param signer   - Optional signer for authenticated methods
   * @param options  - Override defaults (rpcUrl, contractId)
   *
   * @example
   * ```ts
   * const client = ILNClient.testnet(freighterSigner);
   * ```
   */
  static testnet(
    signer?: ISigner,
    options?: { rpcUrl?: string; contractId?: string; contracts?: Partial<ContractAddresses> }
  ): ILNClient {
    const registry = CONTRACT_REGISTRY.testnet;
    return new ILNClient({
      rpcUrl: options?.rpcUrl ?? TESTNET_RPC_URL,
      networkPassphrase: "Test SDF Network ; September 2015",
      contractId: options?.contractId ?? registry.invoiceLiquidity,
      contracts: { ...registry, ...options?.contracts },
      ...(signer ? { signer } : {}),
    });
  }

  /**
   * Create a client pre-configured for Stellar Mainnet (Pubnet).
   *
   * Fails loudly if mainnet contract addresses are not yet populated.
   * Pass `options.contractId` to override the default registry lookup.
   *
   * @param signer   - Optional signer for authenticated methods
   * @param options  - Override defaults (rpcUrl, contractId)
   *
   * @example
   * ```ts
   * const client = ILNClient.mainnet(freighterSigner);
   * ```
   */
  static mainnet(
    signer?: ISigner,
    options?: { rpcUrl?: string; contractId?: string; contracts?: Partial<ContractAddresses> }
  ): ILNClient {
    const registry = CONTRACT_REGISTRY.mainnet;
    const contractId = options?.contractId ?? registry.invoiceLiquidity;
    if (!contractId) {
      throw new Error(
        "Mainnet contract IDs are not yet populated in the registry. " +
          "Pass an explicit contractId or contracts option, or wait for " +
          "mainnet deployment to complete."
      );
    }
    return new ILNClient({
      rpcUrl: options?.rpcUrl ?? MAINNET_RPC_URL,
      networkPassphrase: "Public Global Stellar Network ; September 2015",
      contractId,
      contracts: { ...registry, ...options?.contracts },
      ...(signer ? { signer } : {}),
    });
  }

  /**
   * Create a client with fully custom configuration.
   *
   * Use this for local development (standalone network), Futurenet, or
   * private Stellar deployments.
   *
   * @param config - Full ILNClientConfig
   */
  static custom(config: ILNClientConfig): ILNClient {
    return new ILNClient(config);
  }

  // --------------------------------------------------------------------------
  // Methods
  // --------------------------------------------------------------------------

  /**
   * Fetch the detailed reputation profile for an address.
   *
   * Read-only; does not require a signer.
   *
   * @param address - Stellar G… address to query
   * @returns ReputationProfile (zeroed for unknown addresses)
   */
  async getReputation(
    address: string
  ): Promise<import("./methods/reputation.js").ReputationProfile> {
    if (!this._getReputation) {
      this._getReputation = (await import("./methods/reputation.js"))
        .getReputation;
    }
    return this._getReputation(this.rpc, this.contractId, address, this.networkPassphrase);
  }

  /**
   * Fetch protocol-wide statistics.
   *
   * Read-only; does not require a signer.
   *
   * @returns ContractStats
   */
  async getContractStats(): Promise<
    import("./methods/stats.js").ContractStats
  > {
    if (!this._getContractStats) {
      this._getContractStats = (await import("./methods/stats.js"))
        .getContractStats;
    }
    return this._getContractStats(this.rpc, this.contractId, this.networkPassphrase);
  }

  /**
   * Fetch the top payers leaderboard.
   *
   * Read-only; does not require a signer.
   *
   * @param limit - Maximum number of entries to return (default 10)
   * @returns Array of TopPayerEntry sorted by descending score
   */
  async getTopPayers(
    limit: number = 10
  ): Promise<import("./methods/topPayers.js").TopPayerEntry[]> {
    if (!this._getTopPayers) {
      this._getTopPayers = (await import("./methods/topPayers.js"))
        .getTopPayers;
    }
    return this._getTopPayers(this.rpc, this.contractId, limit, this.networkPassphrase);
  }

  /**
   * Fetch a page of invoices funded by a liquidity provider.
   *
   * Read-only; does not require a signer.
   *
   * @param lp       - Stellar G… address of the liquidity provider
   * @param page     - Zero-indexed page number (default 0)
   * @param pageSize - Number of invoices per page (default 10, capped at 50 by the contract)
   * @returns Array of invoices for the requested page
   */
  async getLpInvoices(
    lp: string,
    page: number = 0,
    pageSize: number = 10
  ): Promise<import("@invoice-liquidity/types").Invoice[]> {
    if (!this._getLpInvoices) {
      this._getLpInvoices = (await import("./methods/lpInvoices.js"))
        .getLpInvoices;
    }
    return this._getLpInvoices(this.rpc, this.contractId, lp, page, pageSize, this.networkPassphrase);
  }

  /**
   * Fetch the current insurance pool balance.
   *
   * @param insurancePoolContractId - Deployed insurance pool contract address
   */
  async getInsurancePoolBalance(
    insurancePoolContractId: string
  ): Promise<bigint> {
    if (!this._getPoolBalance) {
      this._getPoolBalance = (await import("./methods/insurance.js")).getPoolBalance;
    }
    return this._getPoolBalance(this.rpc, insurancePoolContractId, this.networkPassphrase);
  }

  /**
   * Fetch the configured insurance coverage cap.
   *
   * @param insurancePoolContractId - Deployed insurance pool contract address
   */
  async getInsurancePoolCoverage(
    insurancePoolContractId: string
  ): Promise<bigint> {
    if (!this._getCoverage) {
      this._getCoverage = (await import("./methods/insurance.js")).getCoverage;
    }
    return this._getCoverage(this.rpc, insurancePoolContractId, this.networkPassphrase);
  }

  /**
   * Check if a liquidity provider is enrolled in the insurance pool.
   *
   * @param insurancePoolContractId - Deployed insurance pool contract address
   * @param lpAddress - LP's Stellar address
   */
  async isInsurancePoolEnrolled(
    insurancePoolContractId: string,
    lpAddress: string
  ): Promise<boolean> {
    if (!this._isEnrolled) {
      this._isEnrolled = (await import("./methods/insurance.js")).isEnrolled;
    }
    return this._isEnrolled(this.rpc, insurancePoolContractId, lpAddress, this.networkPassphrase);
  }

  /**
   * Fetch the total premiums paid by an LP.
   *
   * @param insurancePoolContractId - Deployed insurance pool contract address
   * @param lpAddress - LP's Stellar address
   */
  async getInsurancePoolPremiumsPaid(
    insurancePoolContractId: string,
    lpAddress: string
  ): Promise<bigint> {
    if (!this._getPremiumsPaid) {
      this._getPremiumsPaid = (await import("./methods/insurance.js")).getPremiumsPaid;
    }
    return this._getPremiumsPaid(this.rpc, insurancePoolContractId, lpAddress, this.networkPassphrase);
  }

  /**
   * Fetch all insurance pool info for an LP.
   *
   * @param insurancePoolContractId - Deployed insurance pool contract address
   * @param lpAddress - LP's Stellar address
   */
  async getInsurancePoolInfo(
    insurancePoolContractId: string,
    lpAddress: string
  ): Promise<import("@invoice-liquidity/types").InsurancePoolInfo> {
    if (!this._getInsurancePoolInfo) {
      this._getInsurancePoolInfo = (await import("./methods/insurance.js")).getInsurancePoolInfo;
    }
    return this._getInsurancePoolInfo(this.rpc, insurancePoolContractId, lpAddress, this.networkPassphrase);
  }

  /**
   * Get the insurance pool health snapshot.
   *
   * @param insurancePoolContractId - Deployed insurance pool contract address
   */
  async getPoolHealth(insurancePoolContractId: string) {
    if (!this._getPoolHealth) {
      this._getPoolHealth = (await import("./methods/insurance.js")).getPoolHealth;
    }
    return this._getPoolHealth(this.rpc, insurancePoolContractId, this.networkPassphrase);
  }

  /**
   * Fetch a participant's accrued distribution tokens.
   *
   * @param distributionContractId - Deployed distribution contract address
   * @param participantAddress - Stellar address of the participant
   */
  async getDistributionAccrual(
    distributionContractId: string,
    participantAddress: string
  ): Promise<number> {
    if (!this._getDistributionAccrual) {
      this._getDistributionAccrual = (await import("./methods/distribution.js")).getDistributionAccrual;
    }
    return this._getDistributionAccrual(this.rpc, distributionContractId, participantAddress, this.networkPassphrase);
  }

  /**
   * Submit a batch of contract calls as a single atomic transaction.
   *
   * Combines multiple contract invocations into one transaction, improving UX
   * and reducing gas costs. All operations succeed or fail together.
   *
   * Requires a signer for automatic submission. Returns the transaction hash
   * if successfully submitted to the network.
   *
   * @param calls - Array of contract call specifications
   * @returns Transaction hash submitted to the network
   *
   * @throws If the batch simulation fails or submission is rejected
   *
   * @example
   * ```ts
   * const txHash = await client.submitBatchTransaction([
   *   {
   *     contractId: client.contractId,
   *     method: "submit_invoice",
   *     args: [freelancer, payer, token, amount, dueDate, discountRate],
   *   },
   *   {
   *     contractId: client.contractId,
   *     method: "join_fund_queue",
   *     args: [invoiceId],
   *   },
   * ]);
   * ```
   */
  async submitBatchTransaction(
    calls: import("./methods/batch.js").BatchContractCall[]
  ): Promise<string> {
    if (!this.signer) {
      throw new Error("Batch transaction submission requires a signer");
    }
    // Verify network passphrase before submitting any transaction
    if (!this._networkVerified) {
      await this.verifyNetwork();
      this._networkVerified = true;
    }
    if (!this._submitBatchTransaction) {
      this._submitBatchTransaction = (await import("./methods/batch.js")).submitBatchTransaction;
    }
    return this._submitBatchTransaction(calls, this.rpc, this.signer.getPublicKey(), this.signer, {
      networkPassphrase: this.networkPassphrase,
    });
  }
}

// ---------------------------------------------------------------------------
// Singleton
// ---------------------------------------------------------------------------

/**
 * Default ILNClient singleton.
 *
 * Must be initialised via `iln.configure(...)` before use.
 *
 * @example
 * ```ts
 * import { iln } from "@iln/sdk";
 *
 * iln.configure({ rpcUrl: "...", networkPassphrase: Networks.TESTNET, contractId: "..." });
 * await iln.getReputation("G...");
 * ```
 */
class ILNSingleton {
  private _client: ILNClient | null = null;

  configure(config: ILNClientConfig): void {
    this._client = new ILNClient(config);
  }

  /** Access the underlying client. Throws if not configured. */
  get client(): ILNClient {
    if (!this._client) {
      throw new Error(
        "ILN singleton not configured. Call iln.configure({...}) first."
      );
    }
    return this._client;
  }

  async getReputation(address: string) {
    return this.client.getReputation(address);
  }

  async getContractStats() {
    return this.client.getContractStats();
  }

  async getTopPayers(limit: number = 10) {
    return this.client.getTopPayers(limit);
  }

  async getLpInvoices(lp: string, page: number = 0, pageSize: number = 10) {
    return this.client.getLpInvoices(lp, page, pageSize);
  }

  async getInsurancePoolBalance(insurancePoolContractId: string) {
    return this.client.getInsurancePoolBalance(insurancePoolContractId);
  }

  async getInsurancePoolCoverage(insurancePoolContractId: string) {
    return this.client.getInsurancePoolCoverage(insurancePoolContractId);
  }

  async isInsurancePoolEnrolled(insurancePoolContractId: string, lpAddress: string) {
    return this.client.isInsurancePoolEnrolled(insurancePoolContractId, lpAddress);
  }

  async getInsurancePoolPremiumsPaid(insurancePoolContractId: string, lpAddress: string) {
    return this.client.getInsurancePoolPremiumsPaid(insurancePoolContractId, lpAddress);
  }

  async getInsurancePoolInfo(insurancePoolContractId: string, lpAddress: string) {
    return this.client.getInsurancePoolInfo(insurancePoolContractId, lpAddress);
  }

  async getPoolHealth(insurancePoolContractId: string) {
    return this.client.getPoolHealth(insurancePoolContractId);
  }

  async getDistributionAccrual(distributionContractId: string, participantAddress: string) {
    return this.client.getDistributionAccrual(distributionContractId, participantAddress);
  }

  async submitBatchTransaction(calls: import("./methods/batch.js").BatchContractCall[]) {
    return this.client.submitBatchTransaction(calls);
  }

  async verifyNetwork(): Promise<void> {
    return this.client.verifyNetwork();
  }
}

export const iln = new ILNSingleton();
