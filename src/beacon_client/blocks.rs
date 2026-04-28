use crate::beacon_client::types::{
    BeaconHeaderData, RawBlockResponse, RawBlockRootResponse, SignedBeaconBlock,
};
use crate::error::{Error, Result};

use super::BeaconClient;
use super::types::{BlockId, BlockRoot};

impl BeaconClient {
    async fn get_block_with_finalized(
        &self,
        block_id: &BlockId,
    ) -> Result<(Option<SignedBeaconBlock>, bool)> {
        let path = format!("/eth/v2/beacon/blocks/{}", block_id.as_request_segment());
        let response = match self.get_response(&path).await {
            Ok(resp) => resp,
            Err(Error::BeaconApi { status: 404, .. }) => return Ok((None, false)),
            Err(e) => return Err(e),
        };

        let raw: RawBlockResponse = response.json().await.map_err(Error::Http)?;
        let (block, finalized) = raw.into_parts();
        Ok((Some(block), finalized))
    }

    pub async fn get_head_slot(&self) -> Result<u64> {
        if let Some((slot, when)) = *self.head_slot_cache.read().await
            && when.elapsed() < super::HEAD_SLOT_TTL
        {
            crate::metrics::record_cache("head_slot", true);
            return Ok(slot);
        }
        crate::metrics::record_cache("head_slot", false);
        let response: BeaconHeaderData = self.get("/eth/v1/beacon/headers/head").await?;
        let slot = response.header.message.slot;
        *self.head_slot_cache.write().await = Some((slot, std::time::Instant::now()));
        Ok(slot)
    }

    /// Fetch a block by beacon block id (slot, root, "head", "genesis", "finalized").
    /// Returns None on 404.
    /// Cache policy:
    /// - root -> block is cached for every successful fetch where root is known.
    /// - slot -> root is cached only when the fetched slot lookup is finalized.
    /// - head/genesis/finalized lookups are never cached.
    pub async fn get_block<I>(&self, block_id: I) -> Result<(Option<SignedBeaconBlock>, bool)>
    where
        I: TryInto<BlockId, Error = Error>,
    {
        let block_id = block_id.try_into()?;

        match block_id {
            BlockId::Slot(slot) => {
                let (slot_root, slot_root_finalized) = self.get_block_root(slot).await?;

                if let Some(root) = slot_root.clone() {
                    let cached_block = {
                        let mut root_cache = self.root_block_cache.lock().await;
                        root_cache.get(&root).cloned()
                    };

                    if let Some(block) = cached_block {
                        crate::metrics::record_cache("root_block", true);
                        tracing::trace!(slot = slot, root = %root, "Beacon client slot->root cache hit");
                        return Ok((Some(block), slot_root_finalized));
                    }
                    crate::metrics::record_cache("root_block", false);
                }

                let slot_id = BlockId::Slot(slot);
                let (block, is_finalized) = self.get_block_with_finalized(&slot_id).await?;

                if let Some(ref fetched_block) = block {
                    if let Some(root) = slot_root {
                        {
                            let mut root_cache = self.root_block_cache.lock().await;
                            root_cache.put(root.clone(), fetched_block.clone());
                        }

                        if is_finalized || slot_root_finalized {
                            let mut slot_cache = self.slot_root_cache.lock().await;
                            slot_cache.put(slot, root);
                        }
                    } else {
                        tracing::warn!(
                            slot = slot,
                            "Block fetched but block root lookup returned none"
                        );
                    }
                }

                Ok((block, is_finalized))
            }
            BlockId::Root(root) => {
                if let Some(cached) = self.root_block_cache.lock().await.get(&root).cloned() {
                    crate::metrics::record_cache("root_block", true);
                    tracing::debug!(root = %root, "Beacon client root cache hit");
                    let cached_finalized = {
                        let slot = cached.slot();
                        let mut slot_cache = self.slot_root_cache.lock().await;
                        slot_cache.get(&slot).map(|r| r == &root).unwrap_or(false)
                    };
                    return Ok((Some(cached), cached_finalized));
                }
                crate::metrics::record_cache("root_block", false);

                let (block, is_finalized) = self
                    .get_block_with_finalized(&BlockId::Root(root.clone()))
                    .await?;

                if let Some(ref b) = block {
                    {
                        let mut root_cache = self.root_block_cache.lock().await;
                        root_cache.put(root.clone(), b.clone());
                    }

                    if is_finalized {
                        let mut slot_cache = self.slot_root_cache.lock().await;
                        slot_cache.put(b.slot(), root);
                    }
                }

                Ok((block, is_finalized))
            }
            id @ (BlockId::Head | BlockId::Genesis | BlockId::Finalized) => {
                self.get_block_with_finalized(&id).await
            }
        }
    }

    /// Fetch block root for a block id. Returns (None, false) on 404.
    pub async fn get_block_root<I>(&self, block_id: I) -> Result<(Option<BlockRoot>, bool)>
    where
        I: TryInto<BlockId, Error = Error>,
    {
        let block_id = block_id.try_into()?;
        if let BlockId::Slot(slot) = block_id {
            if let Some(cached) = self.slot_root_cache.lock().await.get(&slot) {
                crate::metrics::record_cache("slot_root", true);
                return Ok((Some(cached.clone()), true));
            }
            crate::metrics::record_cache("slot_root", false);
        }

        let path = format!(
            "/eth/v1/beacon/blocks/{}/root",
            block_id.as_request_segment()
        );
        let response = match self.get_response(&path).await {
            Ok(resp) => resp,
            Err(Error::BeaconApi { status: 404, .. }) => return Ok((None, false)),
            Err(e) => return Err(e),
        };

        let raw: RawBlockRootResponse = response.json().await.map_err(Error::Http)?;
        let root = raw.data.root;

        if raw.finalized
            && let BlockId::Slot(slot) = block_id
        {
            let mut slot_cache = self.slot_root_cache.lock().await;
            slot_cache.put(slot, root.clone());
        }

        Ok((Some(root), raw.finalized))
    }
}
