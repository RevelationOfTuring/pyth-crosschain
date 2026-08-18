use {
    // 从本 crate 导入：
    // - accounts 模块（包含 UpdatePriceFeed 账户结构体）
    // - instruction 模块（包含 UpdatePriceFeed 指令数据）
    // - PostUpdateParams（指令参数结构体）
    // - ID（本程序的 program ID，即pyth-push-oracle的program id）
    crate::{accounts, instruction, PostUpdateParams, ID},
    // 导入 Anchor 常用类型、system_program 常量、InstructionData trait
    anchor_lang::{prelude::*, system_program, InstructionData},
    // 从 receiver SDK 导入 PDA 地址计算函数
    pyth_solana_receiver_sdk::pda::{get_config_address, get_treasury_address},
    // 导入 FeedId 类型别名（[u8; 32]）和 MerklePriceUpdate 结构体
    pythnet_sdk::{messages::FeedId, wire::v1::MerklePriceUpdate},
    // 导入 Solana 原生 Instruction 类型
    solana_program::instruction::Instruction,
};

// 根据 shard_id 和 feed_id，计算出一个确定性的 PDA 地址，作为该 feed 的价格存储账户
// 注：
//  - 这里使用的是pyth-push-oracle 的 program ID，说明PDA 地址由该程序 ID 派生，因此只有该程序能用 invoke_signed 以这个 PDA 的身份签名
//  - feed_id：Pyth 网络中每个价格 feed 的唯一标识符，是一个 32 字节的哈希值，全网唯一
//  - shard_id：分片编号，是一个 u16（0 ~ 65535），用于将同一 feed 分散到不同 PDA 地址。
//    为什么要分片？
//    答：Solana 的 Sealevel 运行时通过检查交易的读写集来决定哪些交易可以并行执行。
//       如果多笔交易同时写入同一个账户，它们会因为读写集冲突而被迫串行执行。
//       串行执行意味着吞吐量受限于单线程速度，大量交易会排队等待，导致：
//       - 部分交易可能溢出到下一个 slot 才执行
//       - 延迟增加，价格更新不及时
//      通过 shard_id 将同一 feed 的写入分散到不同的 PDA 地址，
//      这些地址的读写集不重叠，可以并行执行，大幅提升吞吐量。
pub fn get_price_feed_address(shard_id: u16, feed_id: FeedId) -> Pubkey {
    Pubkey::find_program_address(&[&shard_id.to_le_bytes(), feed_id.as_ref()], &ID).0
}

// 在客户端（链下）构建 UpdatePriceFeed 账户结构体，为后续构造 Solana指令做准备
impl accounts::UpdatePriceFeed {
    // 为 UpdatePriceFeed 结构体添加方法。accounts 模块由 Anchor 的 #[derive(Accounts)] 宏自动生成，包含 UpdatePriceFeed 的定义
    // populate方法使得调用者只需提供 5 个业务相关的值，其余 4 个字段
    //（config、treasury、price_feed_account、两个常量）由函数自动计算，不需要手动推导 PDA。
    pub fn populate(
        payer: Pubkey,
        // Wormhole 链上已验证的 VAA 账户地址
        encoded_vaa: Pubkey,
        shard_id: u16,
        feed_id: FeedId,
        // 不同的treasury_id对应不同 treasury PDA 账户，用于分散写入负载。
        // 注：每次向链上提交价格更新时，payer 需要支付一笔费用，这笔费用会转入 treasury 账户（目前 treasury 的钱锁在里面取不出来）
        treasury_id: u8,
    ) -> Self {
        accounts::UpdatePriceFeed {
            payer,
            encoded_vaa,
            // 计算 receiver 的 Config PDA 地址
            config: get_config_address(),
            // 计算费用收款账户的 PDA 地址
            treasury: get_treasury_address(treasury_id),
            // 计算价格存储账户的 PDA 地址
            price_feed_account: get_price_feed_address(shard_id, feed_id),
            pyth_solana_receiver: pyth_solana_receiver_sdk::ID,
            system_program: system_program::ID,
        }
    }
}

// 构建一个完整的 Solana Instruction（去调用pyth-push-oracle 程序的 update_price_feed 方法）
impl instruction::UpdatePriceFeed {
    pub fn populate(
        payer: Pubkey,
        encoded_vaa: Pubkey,
        shard_id: u16,
        feed_id: FeedId,
        treasury_id: u8,
        merkle_price_update: MerklePriceUpdate,
    ) -> Instruction {
        // 先构建UpdatePriceFeed账户结构体，再调用 to_account_metas(None)转为 Vec<AccountMeta>
        // 注：
        // - to_account_metas 是 Anchor 的 #[derive(Accounts)] 宏自动生成的方法，将每个字段映射为 AccountMeta
        // - to_account_metas(None)的参数 None 表示没有额外的 signer 覆盖（签名者由结构体自身定义确定，没有额外的 signer 需要覆盖）
        //   传Some的典型场景：在链上 CPI 调用时，PDA 需要作为 signer 签名，但结构体定义里它可能不是 Signer 类型。这时就需要覆盖。
        //   // 链上 CPI 调用时
        //   let account_metas = ctx.accounts.to_account_metas(Some(&[
        //       &[b"treasury", &[bump]]  // 告诉 Anchor：treasury这个账户也是 signer
        //   ]));
        let update_price_feed_accounts =
            accounts::UpdatePriceFeed::populate(payer, encoded_vaa, shard_id, feed_id, treasury_id)
                .to_account_metas(None);
        // 构建Instruction
        Instruction {
            program_id: ID,                       // 告诉Solana运行时由哪个program执行这条指令
            accounts: update_price_feed_accounts, // 签名构建的账户列表（Vec<AccountMeta>）
            // 调用pyth-push-oracle的update_price_feed方法的传参
            // 注：instruction::UpdatePriceFeed 结构体是哪来的？
            // 答：Anchor #[program] 宏自动生成的，源码里找不到定义。
            data: instruction::UpdatePriceFeed {
                params: PostUpdateParams {
                    merkle_price_update,
                    treasury_id,
                },
                shard_id,
                feed_id,
            }
            .data(),
            // 调用 .data() 后，得到的是这样的字节数组：格式：[8 字节选择器] + [Borsh 序列化的指令数据]
        }
    }
}
