import { PublicKey } from "@solana/web3.js";

/** The market PDA, as the Metis program derives it: ["market", creator, nonce as u64 LE]. */
export function marketAddress(core: PublicKey, creator: PublicKey, nonce: bigint): PublicKey {
  const n = Buffer.alloc(8);
  n.writeBigUInt64LE(nonce);
  return PublicKey.findProgramAddressSync([Buffer.from("market"), creator.toBuffer(), n], core)[0];
}
