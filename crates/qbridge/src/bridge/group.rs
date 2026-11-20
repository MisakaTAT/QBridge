//! 群信息、群成员与禁言动作。

use crate::{
    bridge::Bridge,
    error::{ActionError, NotFoundKind, UnsupportedKind},
    store::IdentityKind,
};
use chrono::{DateTime, SecondsFormat};
use qbridge_ob11::action::{
    GetGroupInfo, GetGroupMemberInfo, GetGroupMemberList, SetGroupAddRequest, SetGroupBan,
    SetGroupKick,
};
use qbridge_qq::{GroupOpenId, MemberOpenId, MuteOp};
use serde_json::{Value, json};

pub async fn info(bridge: &Bridge, request: GetGroupInfo) -> Result<Value, ActionError> {
    let openid = group_openid(bridge, request.group_id).await?;
    let info = bridge.qq.group_info(&openid).await?;
    let count = info.group_member_num;
    let mut result = json!({
        "group_id": request.group_id,
        "group_name": info.group_name,
        "qq": info,
    });
    if let Some(count) = count {
        result["member_count"] = Value::from(count);
    }
    Ok(result)
}

pub async fn member(bridge: &Bridge, request: GetGroupMemberInfo) -> Result<Value, ActionError> {
    let group = group_openid(bridge, request.group_id).await?;
    let member = member_openid(bridge, request.user_id).await?;
    let info = bridge.qq.group_member(&group, &member).await?;
    let member_id = bridge
        .store
        .identity(IdentityKind::Member, &info.member_openid)
        .await?;
    bridge
        .store
        .save_profile(
            member_id,
            info.username.as_deref().unwrap_or_default(),
            "",
            "",
            600,
            crate::bridge::unix_now(),
        )
        .await?;
    Ok(json!({
        "group_id": request.group_id,
        "user_id": member_id,
        "nickname": info.username.clone().unwrap_or_default(),
        "role": info.member_role.clone().unwrap_or_else(|| "member".into()),
        "qq": info,
    }))
}

pub async fn members(bridge: &Bridge, request: GetGroupMemberList) -> Result<Value, ActionError> {
    let group = group_openid(bridge, request.group_id).await?;
    let mut raw = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = bridge.qq.group_members(&group, cursor.as_deref()).await?;
        raw.extend(page.members);
        if page.next_cursor.is_empty() {
            break;
        }
        if cursor.as_deref() == Some(page.next_cursor.as_str()) {
            return Err(ActionError::BadParam(
                "QQ group member cursor did not advance".into(),
            ));
        }
        cursor = Some(page.next_cursor);
    }
    let openids = raw
        .iter()
        .map(|member| member.member_openid.clone())
        .collect::<Vec<_>>();
    let ids = bridge
        .store
        .identity_batch(IdentityKind::Member, &openids)
        .await?;
    let profiles = raw
        .iter()
        .filter_map(|member| {
            ids.get(&member.member_openid).map(|id| {
                (
                    *id,
                    member.username.clone().unwrap_or_default(),
                    String::new(),
                    String::new(),
                )
            })
        })
        .collect::<Vec<_>>();
    bridge
        .store
        .save_profiles(&profiles, 600, crate::bridge::unix_now())
        .await?;
    let mut members = Vec::with_capacity(raw.len());
    for member in raw {
        let user_id = *ids
            .get(&member.member_openid)
            .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
        members.push(json!({
            "group_id": request.group_id,
            "user_id": user_id,
            "nickname": member.username.clone().unwrap_or_default(),
            "role": member.member_role.clone().unwrap_or_else(|| "member".into()),
            "qq": member,
        }));
    }
    Ok(Value::Array(members))
}

pub async fn kick(bridge: &Bridge, request: SetGroupKick) -> Result<Value, ActionError> {
    let group = group_openid(bridge, request.group_id).await?;
    let member = member_openid(bridge, request.user_id).await?;
    bridge
        .qq
        .remove_group_members(
            &group,
            &[member],
            request.reject_add_request || request.add_blacklist,
        )
        .await?;
    Ok(Value::Null)
}

pub async fn add_request(
    bridge: &Bridge,
    request: SetGroupAddRequest,
) -> Result<Value, ActionError> {
    if request.sub_type != "add" {
        return Err(ActionError::Unsupported(UnsupportedKind::Action));
    }
    let (group_id, user_id, join_request_id) = parse_join_flag(&request.flag).ok_or_else(|| {
        ActionError::BadParam("flag is not a QBridge group join request flag".into())
    })?;
    let group = group_openid(bridge, group_id).await?;
    let member = member_openid(bridge, user_id).await?;
    let reason = (!request.approve).then_some(request.reason.as_str());
    bridge
        .qq
        .approve_join_request(&group, &member, &join_request_id, request.approve, reason)
        .await?;
    Ok(Value::Null)
}

pub fn join_flag(group_id: i64, user_id: i64, join_request_id: &str) -> String {
    format!("qq_group:{group_id}:{user_id}:{join_request_id}")
}

fn parse_join_flag(flag: &str) -> Option<(i64, i64, String)> {
    let mut parts = flag.splitn(4, ':');
    if parts.next()? != "qq_group" {
        return None;
    }
    let group = parts.next()?.parse().ok()?;
    let user = parts.next()?.parse().ok()?;
    let join_request_id = parts.next()?.to_owned();
    if join_request_id.is_empty() {
        return None;
    }
    Some((group, user, join_request_id))
}

pub async fn ban(bridge: &Bridge, request: SetGroupBan, now: i64) -> Result<Value, ActionError> {
    let group = group_openid(bridge, request.group_id).await?;
    let member = member_openid(bridge, request.user_id).await?;
    let operation = if request.duration == 0 {
        MuteOp::delete(member)
    } else {
        let duration = i64::try_from(request.duration)
            .map_err(|_| ActionError::BadParam("invalid ban duration".into()))?;
        let expires_at = now
            .checked_add(duration)
            .ok_or_else(|| ActionError::BadParam("invalid ban duration".into()))?;
        let expires = DateTime::from_timestamp(expires_at, 0)
            .ok_or_else(|| ActionError::BadParam("invalid ban duration".into()))?
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        MuteOp::add(member, expires)
    };
    bridge.qq.set_member_mute(&group, &[operation]).await?;
    Ok(Value::Null)
}

pub(super) async fn group_openid(
    bridge: &Bridge,
    group_id: i64,
) -> Result<GroupOpenId, ActionError> {
    let openid = bridge
        .store
        .openid(IdentityKind::Group, group_id)
        .await?
        .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
    GroupOpenId::new(openid).map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget))
}

pub(super) async fn member_openid(
    bridge: &Bridge,
    user_id: i64,
) -> Result<MemberOpenId, ActionError> {
    let openid = bridge
        .store
        .openid(IdentityKind::Member, user_id)
        .await?
        .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
    MemberOpenId::new(openid).map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget))
}
