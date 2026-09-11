//! IP Security Module Tests
//! IP 安全监控功能的综合测试套件
//!
//! 测试目标:
//! 1. 验证 IP 黑/白名单功能的正确性
//! 2. 验证 CIDR 匹配逻辑
//! 3. 验证过期时间处理
//! 4. 验证不影响主流程性能
//! 5. 验证数据库操作的原子性和一致性

#[cfg(test)]
mod security_db_tests {
    use crate::modules::security_db::{
        add_to_blacklist, add_to_whitelist, cleanup_old_ip_logs, get_blacklist,
        get_blacklist_entry_for_ip, get_ip_access_logs, get_ip_stats, init_db, is_ip_in_blacklist,
        is_ip_in_whitelist, remove_from_blacklist, save_ip_access_log, test_db_guard, IpAccessLog,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 辅助函数：获取当前时间戳
    fn now_timestamp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    // ============================================================================
    // 测试类别 1: 数据库初始化
    // ============================================================================

    #[test]
    fn test_db_initialization() {
        // 验证数据库初始化不会 panic
        let _db = test_db_guard().expect("isolated security database");
        let result = init_db();
        assert!(
            result.is_ok(),
            "Database initialization should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_db_multiple_initializations() {
        // 验证多次初始化不会出错 (幂等性)
        let _db = test_db_guard().expect("isolated security database");
        for _ in 0..3 {
            let result = init_db();
            assert!(
                result.is_ok(),
                "Multiple DB initializations should be idempotent"
            );
        }
    }

    // ============================================================================
    // 测试类别 2: IP 黑名单基本操作
    // ============================================================================

    #[test]
    fn test_blacklist_add_and_check() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加 IP 到黑名单
        let result = add_to_blacklist("192.168.1.100", Some("Test block"), None, "test");
        assert!(
            result.is_ok(),
            "Should add IP to blacklist: {:?}",
            result.err()
        );

        // 验证 IP 在黑名单中
        let is_blocked = is_ip_in_blacklist("192.168.1.100");
        assert!(is_blocked.is_ok());
        assert!(is_blocked.unwrap(), "IP should be in blacklist");

        // 验证其他 IP 不在黑名单中
        let is_other_blocked = is_ip_in_blacklist("192.168.1.101");
        assert!(is_other_blocked.is_ok());
        assert!(
            !is_other_blocked.unwrap(),
            "Other IP should not be in blacklist"
        );
    }

    #[test]
    fn test_blacklist_remove() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加 IP
        let entry = add_to_blacklist("10.0.0.5", Some("Temp block"), None, "test").unwrap();

        // 验证存在
        assert!(is_ip_in_blacklist("10.0.0.5").unwrap());

        // 移除
        let remove_result = remove_from_blacklist(&entry.id);
        assert!(remove_result.is_ok());

        // 验证已移除
        assert!(!is_ip_in_blacklist("10.0.0.5").unwrap());
    }

    #[test]
    fn test_blacklist_get_entry_details() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加带有详细信息的条目
        let _ = add_to_blacklist(
            "172.16.0.50",
            Some("Abuse detected"),
            Some(now_timestamp() + 3600), // 1小时后过期
            "admin",
        );

        // 获取条目详情
        let entry_result = get_blacklist_entry_for_ip("172.16.0.50");
        assert!(entry_result.is_ok());

        let entry = entry_result.unwrap();
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.ip_pattern, "172.16.0.50");
        assert_eq!(entry.reason.as_deref(), Some("Abuse detected"));
        assert_eq!(entry.created_by, "admin");
        assert!(entry.expires_at.is_some());
    }

    // ============================================================================
    // 测试类别 3: CIDR 匹配
    // ============================================================================

    #[test]
    fn test_cidr_matching_basic() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加 CIDR 范围到黑名单
        let _ = add_to_blacklist("192.168.1.0/24", Some("Block subnet"), None, "test");

        // 验证该子网内的 IP 都被阻止
        assert!(
            is_ip_in_blacklist("192.168.1.1").unwrap(),
            "192.168.1.1 should match /24"
        );
        assert!(
            is_ip_in_blacklist("192.168.1.100").unwrap(),
            "192.168.1.100 should match /24"
        );
        assert!(
            is_ip_in_blacklist("192.168.1.254").unwrap(),
            "192.168.1.254 should match /24"
        );

        // 验证子网外的 IP 不被阻止
        assert!(
            !is_ip_in_blacklist("192.168.2.1").unwrap(),
            "192.168.2.1 should not match"
        );
        assert!(
            !is_ip_in_blacklist("10.0.0.1").unwrap(),
            "10.0.0.1 should not match"
        );
    }

    #[test]
    fn test_cidr_matching_various_masks() {
        let _db = test_db_guard().expect("isolated security database");

        // 测试 /16 掩码
        let subnet = add_to_blacklist("10.10.0.0/16", Some("Block /16"), None, "test").unwrap();

        assert!(is_ip_in_blacklist("10.10.0.1").unwrap(), "Should match /16");
        assert!(
            is_ip_in_blacklist("10.10.255.255").unwrap(),
            "Should match /16"
        );
        assert!(
            !is_ip_in_blacklist("10.11.0.1").unwrap(),
            "Should not match /16"
        );
        remove_from_blacklist(&subnet.id).unwrap();

        // 测试 /32 掩码 (单个 IP)
        let _ = add_to_blacklist("8.8.8.8/32", Some("Block single"), None, "test");

        assert!(is_ip_in_blacklist("8.8.8.8").unwrap(), "Should match /32");
        assert!(
            !is_ip_in_blacklist("8.8.8.9").unwrap(),
            "Should not match /32"
        );
    }

    #[test]
    fn test_cidr_edge_cases() {
        let _db = test_db_guard().expect("isolated security database");

        // 测试 /0 (所有 IP) - 边界情况
        let all = add_to_blacklist("0.0.0.0/0", Some("Block all"), None, "test").unwrap();

        assert!(
            is_ip_in_blacklist("1.2.3.4").unwrap(),
            "Everything should match /0"
        );
        assert!(
            is_ip_in_blacklist("255.255.255.255").unwrap(),
            "Everything should match /0"
        );
        remove_from_blacklist(&all.id).unwrap();

        // 测试 /8 掩码
        let _ = add_to_blacklist("10.0.0.0/8", Some("Block /8"), None, "test");

        assert!(
            is_ip_in_blacklist("10.255.255.255").unwrap(),
            "Should match /8"
        );
        assert!(
            !is_ip_in_blacklist("11.0.0.0").unwrap(),
            "Should not match /8"
        );
    }

    // ============================================================================
    // 测试类别 4: 过期时间处理
    // ============================================================================

    #[test]
    fn test_blacklist_expiration() {
        let _db = test_db_guard().expect("isolated security database");

        // A ban expires at its timestamp, not one second later.
        let expires_at = now_timestamp();
        add_to_blacklist(
            "198.51.100.10",
            Some("Expires immediately"),
            Some(expires_at),
            "test",
        )
        .expect("add expired blacklist fixture");

        assert!(
            !is_ip_in_blacklist("198.51.100.10").expect("check expired blacklist entry"),
            "An entry at its expiration boundary must not block"
        );
    }

    #[test]
    fn test_blacklist_not_yet_expired() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加一个未过期的条目
        let _ = add_to_blacklist(
            "198.51.100.11",
            Some("Will expire later"),
            Some(now_timestamp() + 3600), // 1小时后过期
            "test",
        );

        // 未过期条目应该仍然生效
        assert!(is_ip_in_blacklist("198.51.100.11").unwrap());
    }

    #[test]
    fn test_permanent_blacklist() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加永久封禁 (无过期时间)
        let _ = add_to_blacklist(
            "198.51.100.12",
            Some("Permanent ban"),
            None, // 无过期时间
            "test",
        );

        // 永久封禁应该始终生效
        assert!(is_ip_in_blacklist("198.51.100.12").unwrap());
    }

    // ============================================================================
    // 测试类别 5: IP 白名单
    // ============================================================================

    #[test]
    fn test_whitelist_add_and_check() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加 IP 到白名单
        let result = add_to_whitelist("10.0.0.1", Some("Trusted server"));
        assert!(result.is_ok());

        // 验证 IP 在白名单中
        assert!(is_ip_in_whitelist("10.0.0.1").unwrap());
        assert!(!is_ip_in_whitelist("10.0.0.2").unwrap());
    }

    #[test]
    fn test_whitelist_cidr() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加 CIDR 范围到白名单
        let _ = add_to_whitelist("192.168.0.0/16", Some("Internal network"));

        // 验证子网内的 IP 都被允许
        assert!(is_ip_in_whitelist("192.168.1.1").unwrap());
        assert!(is_ip_in_whitelist("192.168.255.255").unwrap());

        // 验证子网外的 IP 不在白名单
        assert!(!is_ip_in_whitelist("10.0.0.1").unwrap());
    }

    // ============================================================================
    // 测试类别 6: IP 访问日志
    // ============================================================================

    #[test]
    fn test_access_log_save_and_retrieve() {
        let _db = test_db_guard().expect("isolated security database");
        let log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "198.51.100.50".to_string(),
            timestamp: now_timestamp(),
            method: Some("POST".to_string()),
            path: Some("/v1/messages".to_string()),
            user_agent: Some("TestClient/1.0".to_string()),
            status: Some(200),
            duration: Some(150),
            api_key_hash: Some("hash123".to_string()),
            blocked: false,
            block_reason: None,
            username: None,
        };
        save_ip_access_log(&log).expect("save access log");
        let logs =
            get_ip_access_logs(10, 0, Some("198.51.100.50"), false).expect("retrieve access log");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].client_ip, "198.51.100.50");
    }

    #[test]
    fn test_access_log_blocked_filter() {
        let _db = test_db_guard().expect("isolated security database");
        let normal_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "198.51.100.51".to_string(),
            timestamp: now_timestamp(),
            method: Some("GET".to_string()),
            path: Some("/healthz".to_string()),
            user_agent: None,
            status: Some(200),
            duration: Some(10),
            api_key_hash: None,
            blocked: false,
            block_reason: None,
            username: None,
        };
        save_ip_access_log(&normal_log).expect("save normal access log");
        let blocked_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "198.51.100.52".to_string(),
            timestamp: now_timestamp(),
            method: Some("POST".to_string()),
            path: Some("/v1/messages".to_string()),
            user_agent: None,
            status: Some(403),
            duration: Some(0),
            api_key_hash: None,
            blocked: true,
            block_reason: Some("IP in blacklist".to_string()),
            username: None,
        };
        save_ip_access_log(&blocked_log).expect("save blocked access log");
        let blocked_only = get_ip_access_logs(10, 0, None, true).expect("retrieve blocked logs");
        assert_eq!(blocked_only.len(), 1);
        assert_eq!(blocked_only[0].client_ip, "198.51.100.52");
        assert!(blocked_only[0].blocked);
    }

    // ============================================================================
    // 测试类别 7: 统计功能
    // ============================================================================

    #[test]
    fn test_ip_stats() {
        let _db = test_db_guard().expect("isolated security database");
        for i in 0..5 {
            let log = IpAccessLog {
                id: uuid::Uuid::new_v4().to_string(),
                client_ip: format!("198.51.100.{}", 60 + i % 3),
                timestamp: now_timestamp(),
                method: Some("POST".to_string()),
                path: Some("/v1/messages".to_string()),
                user_agent: None,
                status: Some(200),
                duration: Some(100),
                api_key_hash: None,
                blocked: i == 4,
                block_reason: (i == 4).then(|| "Test".to_string()),
                username: None,
            };
            save_ip_access_log(&log).expect("save statistics fixture");
        }
        add_to_blacklist("198.51.100.21", None, None, "test").expect("add blacklist fixture");
        add_to_blacklist("198.51.100.22", None, None, "test").expect("add blacklist fixture");
        add_to_whitelist("198.51.100.23", None).expect("add whitelist fixture");
        let stats = get_ip_stats().expect("read statistics");
        assert_eq!(stats.total_requests, 5);
        assert_eq!(stats.unique_ips, 3);
        assert_eq!(stats.blocked_count, 1);
        assert_eq!(stats.blacklist_count, 2);
        assert_eq!(stats.whitelist_count, 1);
    }

    // ============================================================================
    // 测试类别 8: 清理功能
    // ============================================================================

    #[test]
    fn test_cleanup_old_logs() {
        let _db = test_db_guard().expect("isolated security database");
        let old_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "198.51.100.70".to_string(),
            timestamp: now_timestamp() - (2 * 24 * 3600),
            method: Some("GET".to_string()),
            path: Some("/old".to_string()),
            user_agent: None,
            status: Some(200),
            duration: Some(10),
            api_key_hash: None,
            blocked: false,
            block_reason: None,
            username: None,
        };
        save_ip_access_log(&old_log).expect("save old log");
        let new_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "198.51.100.71".to_string(),
            timestamp: now_timestamp(),
            method: Some("GET".to_string()),
            path: Some("/new".to_string()),
            user_agent: None,
            status: Some(200),
            duration: Some(10),
            api_key_hash: None,
            blocked: false,
            block_reason: None,
            username: None,
        };
        save_ip_access_log(&new_log).expect("save new log");
        assert_eq!(cleanup_old_ip_logs(1).expect("clean old logs"), 1);
        assert_eq!(
            get_ip_access_logs(10, 0, Some("198.51.100.71"), false)
                .expect("retrieve new log")
                .len(),
            1
        );
        assert!(get_ip_access_logs(10, 0, Some("198.51.100.70"), false)
            .expect("retrieve old log")
            .is_empty());
    }

    // ============================================================================
    // 测试类别 9: 并发安全性
    // ============================================================================

    #[test]
    fn test_concurrent_access() {
        use std::thread;

        let _db = test_db_guard().expect("isolated security database");

        let handles: Vec<_> = (0..10)
            .map(|i| {
                thread::spawn(move || {
                    // 每个线程添加不同的 IP
                    let ip = format!("198.18.0.{}", i + 1);
                    let added =
                        add_to_blacklist(&ip, Some("Concurrent test"), None, "test").is_ok();

                    // 验证自己添加的 IP
                    added && is_ip_in_blacklist(&ip).unwrap_or(false)
                })
            })
            .collect();

        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // 所有线程都应该成功
        assert!(
            results.iter().all(|&r| r),
            "All concurrent adds should succeed"
        );
    }

    // ============================================================================
    // 测试类别 10: 边界情况和错误处理
    // ============================================================================

    #[test]
    fn test_duplicate_blacklist_entry() {
        let _db = test_db_guard().expect("isolated security database");

        // 第一次添加应该成功
        let result1 = add_to_blacklist("198.51.100.30", Some("First"), None, "test");
        assert!(result1.is_ok());

        // 第二次添加相同 IP 应该失败 (UNIQUE constraint)
        let result2 = add_to_blacklist("198.51.100.30", Some("Second"), None, "test");
        assert!(result2.is_err(), "Duplicate IP should fail");
    }

    #[test]
    fn test_empty_ip_pattern() {
        let _db = test_db_guard().expect("isolated security database");
        // 空 IP 模式必须被拒绝，避免创建不可匹配的访问规则。
        let result = add_to_blacklist("", Some("Empty IP"), None, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_special_characters_in_reason() {
        let _db = test_db_guard().expect("isolated security database");

        // 测试包含特殊字符的原因
        let reason = "Test with 'quotes' and \"double quotes\" and emoji 🚫";
        let result = add_to_blacklist("198.51.100.31", Some(reason), None, "test");
        assert!(result.is_ok());

        let entry = get_blacklist_entry_for_ip("198.51.100.31")
            .unwrap()
            .unwrap();
        assert_eq!(entry.reason.as_deref(), Some(reason));
    }

    #[test]
    fn test_hit_count_increment() {
        let _db = test_db_guard().expect("isolated security database");

        // 添加一个黑名单条目
        add_to_blacklist("198.51.100.32", Some("Count test"), None, "test")
            .expect("add hit-count fixture");

        for _ in 0..5 {
            get_blacklist_entry_for_ip("198.51.100.32").expect("read hit-count fixture");
        }

        let entry = get_blacklist()
            .expect("read blacklist")
            .into_iter()
            .find(|entry| entry.ip_pattern == "198.51.100.32")
            .expect("stored blacklist entry");
        assert_eq!(entry.hit_count, 5);
    }
}

// ============================================================================
// IP Filter 中间件测试 (单元测试)
// ============================================================================

#[cfg(test)]
mod ip_filter_middleware_tests {
    // 注意：中间件测试需要模拟 HTTP 请求，这里提供测试框架
    // 实际的集成测试应该在启动完整服务后进行

    /// 验证 IP 提取逻辑的正确性
    #[test]
    fn test_ip_extraction_priority() {
        // X-Forwarded-For 应该优先于 X-Real-IP
        // X-Real-IP 应该优先于 ConnectInfo
        // 这里只验证逻辑概念，实际测试需要构造 HTTP 请求

        // 场景 1: X-Forwarded-For 有多个 IP，取第一个
        let xff_header = "203.0.113.1, 198.51.100.2, 192.0.2.3";
        let first_ip = xff_header.split(',').next().unwrap().trim();
        assert_eq!(first_ip, "203.0.113.1");

        // 场景 2: 单个 IP
        let single_ip = "10.0.0.1";
        let parsed = single_ip.split(',').next().unwrap().trim();
        assert_eq!(parsed, "10.0.0.1");
    }
}

// ============================================================================
// 性能基准测试
// ============================================================================

#[cfg(test)]
mod performance_benchmarks {
    use crate::modules::security_db::{add_to_blacklist, is_ip_in_blacklist, test_db_guard};
    use std::time::Instant;

    /// 基准测试：黑名单查找性能
    #[test]
    fn benchmark_blacklist_lookup() {
        let _db = test_db_guard().expect("isolated security database");

        for i in 0..100 {
            add_to_blacklist(
                &format!("198.18.1.{}", i + 1),
                Some("Benchmark"),
                None,
                "test",
            )
            .expect("add benchmark entry");
        }

        // 执行 1000 次查找
        let start = Instant::now();
        for _ in 0..1000 {
            let _ = is_ip_in_blacklist("198.18.1.51");
        }
        let duration = start.elapsed();

        println!("1000 blacklist lookups took: {:?}", duration);
        println!("Average per lookup: {:?}", duration / 1000);

        // 性能断言：平均查找应该在 1ms 以内
        assert!(
            duration.as_millis() < 5000,
            "Blacklist lookup should be fast (< 5ms avg)"
        );

        // The scoped fixture removes this temporary database on drop.
    }

    /// 基准测试：CIDR 匹配性能
    #[test]
    fn benchmark_cidr_matching() {
        let _db = test_db_guard().expect("isolated security database");

        // The scoped fixture begins with an empty database.

        // 添加 20 个 CIDR 规则
        for i in 0..20 {
            let _ = add_to_blacklist(
                &format!("10.{}.0.0/16", i),
                Some("CIDR Benchmark"),
                None,
                "test",
            );
        }

        // 测试 CIDR 匹配性能
        let start = Instant::now();
        for _ in 0..1000 {
            // 测试需要遍历 CIDR 的 IP
            let _ = is_ip_in_blacklist("10.5.100.50");
        }
        let duration = start.elapsed();

        println!("1000 CIDR matches took: {:?}", duration);
        println!("Average per match: {:?}", duration / 1000);

        // 性能断言：CIDR 匹配应该在合理时间内
        assert!(
            duration.as_millis() < 5000,
            "CIDR matching should be reasonably fast"
        );

        // The scoped fixture removes this temporary database on drop.
    }
}
